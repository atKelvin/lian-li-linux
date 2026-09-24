use super::{runtime::ActiveTarget, ServiceManager};
use anyhow::{ensure, Context, Result};
use lianli_shared::startup_image::{StartupImageCapabilities, StartupImageState};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::JoinHandle;
use std::time::Duration;

fn journal_path(config: &std::path::Path) -> std::path::PathBuf {
    config.with_extension("startup-upload.json")
}

pub(super) const RECOVERY_MESSAGE: &str = "Playback paused after an interrupted upload. Clear recovery in Installation Health or restart the daemon. Power-cycle the screen if it remains unresponsive.";

pub(super) fn clear_previous_recovery(config: &std::path::Path) -> Result<()> {
    match std::fs::remove_file(journal_path(config)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("Clearing previous startup upload recovery"),
    }
}

#[cfg(test)]
fn load_quarantine(config: &std::path::Path) -> Result<std::collections::HashSet<String>> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let path = journal_path(config);
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Default::default()),
        Err(error) => return Err(error).context("Reading startup upload recovery journal"),
    };
    ensure!(
        file.metadata()?.is_file(),
        "Startup upload journal is not a regular file"
    );
    let mut data = Vec::new();
    file.take(65_537).read_to_end(&mut data)?;
    ensure!(
        data.len() <= 65_536,
        "Startup upload recovery journal is oversized"
    );
    serde_json::from_slice(&data).context("Invalid startup upload recovery journal")
}

struct Outcome {
    target: Option<ActiveTarget>,
    result: Result<bool>,
    stopped: bool,
    recovery_required: bool,
}

pub(super) struct StartupImageJob {
    id: u64,
    identity: String,
    cancel: Arc<AtomicBool>,
    worker: Option<JoinHandle<Outcome>>,
    _permit: Option<Arc<lianli_control::write_gate::ServiceWritePermit>>,
}

impl Drop for StartupImageJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl ServiceManager {
    pub(super) fn clear_startup_recovery(&mut self, selection: &str) -> Result<()> {
        ensure!(
            self.startup_image_job.is_none(),
            "Wait for the startup upload to finish"
        );
        let selection = selection.strip_prefix("serial:").unwrap_or(selection);
        let identity = self
            .startup_image_quarantine
            .iter()
            .find(|id| super::media::lcd_id_matches(selection, id))
            .cloned()
            .context("This LCD has no startup recovery block")?;
        let mut remaining = self.startup_image_quarantine.clone();
        remaining.remove(&identity);
        crate::persistence::write_json(&journal_path(&self.config_path), &remaining)?;
        self.startup_image_quarantine = remaining;
        self.startup_absent_since.remove(&identity);
        let mut state = self.ipc.state.lock();
        state.state_health.device_opened(&identity);
        for status in state.telemetry.media_preparation.values_mut() {
            if status
                .device_id
                .strip_prefix("serial:")
                .is_some_and(|id| super::media::lcd_id_matches(id, &identity))
                || super::media::lcd_id_matches(&status.device_id, &identity)
            {
                status.startup_recovery_required = false;
                status.error = None;
                status.state = lianli_shared::ipc::MediaPreparationState::WaitingForDevice;
            }
        }
        self.media_reload_pending = true;
        Ok(())
    }
    fn flex_startup_receiver(&self, identity: &str) -> Result<Option<(String, u8)>> {
        let lcd = self
            .registry
            .cached_usb_devices
            .iter()
            .find(|device| device.device_id == identity)
            .context("LCD is no longer in the current USB inventory")?;
        if !matches!(lcd.pid, 0xa018 | 0xa019) || lcd.vid != 0x1cbe {
            return Ok(None);
        }
        let location = self
            .registry
            .usb_locations
            .get(identity)
            .context("Flex LCD USB parent topology is unavailable")?;
        let mut candidates = self.registry.fan_device_info.iter().filter_map(|receiver| {
            if receiver.vid != 0x43a8
                || !self.registry.fan_devices.contains_key(&receiver.device_id)
            {
                return None;
            }
            let receiver_location = self.registry.usb_locations.get(&receiver.device_id)?;
            let slot = flex_receiver_slot(lcd.pid, location, receiver.pid, receiver_location)?;
            Some((receiver.device_id.clone(), slot))
        });
        let receiver = candidates.next().context(
            "Connect the matching Flex LCD receiver on the same USB hub before uploading",
        )?;
        ensure!(
            candidates.next().is_none(),
            "Flex receiver topology is ambiguous; startup upload was not started"
        );
        Ok(Some(receiver))
    }

    pub(super) fn reconcile_startup_quarantine(
        &mut self,
        present: &std::collections::HashSet<String>,
    ) {
        if self.startup_image_job.is_some() {
            return;
        }
        let now = std::time::Instant::now();
        let remaining = self
            .startup_image_quarantine
            .iter()
            .filter(|id| {
                if self
                    .startup_absent_since
                    .get(*id)
                    .is_some_and(|since| now.duration_since(*since) >= Duration::from_secs(10))
                {
                    return false;
                }
                if present.contains(*id) {
                    self.startup_absent_since.remove(*id);
                    true
                } else {
                    self.startup_absent_since
                        .entry((*id).clone())
                        .or_insert(now);
                    true
                }
            })
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        if remaining != self.startup_image_quarantine {
            match crate::persistence::write_json(&journal_path(&self.config_path), &remaining) {
                Ok(()) => {
                    self.startup_absent_since
                        .retain(|id, _| remaining.contains(id));
                    self.startup_image_quarantine = remaining;
                }
                Err(error) => {
                    tracing::warn!(%error, "Could not clear disconnected startup upload quarantine")
                }
            }
        }
    }

    pub(super) fn finish_startup_status(&self, id: u64, status: StartupImageState) {
        let mut state = self.ipc.state.lock();
        if let Some(job) = state.startup_image.as_mut().filter(|job| job.id == id) {
            job.status = status;
            state.startup_image_cancel = None;
        }
    }

    pub(super) fn start_startup_image(
        &mut self,
        id: u64,
        identity: String,
        jpeg: Vec<u8>,
        caps: StartupImageCapabilities,
        cancel: Arc<AtomicBool>,
        permit: Option<Arc<lianli_control::write_gate::ServiceWritePermit>>,
    ) -> Result<()> {
        ensure!(
            self.startup_image_job.is_none()
                && self.display_switch.is_none()
                && !self.lcd_group_recovery.busy(),
            "Another LCD operation is running"
        );
        ensure!(
            !self.startup_image_quarantine.contains(&identity),
            RECOVERY_MESSAGE
        );
        ensure!(
            self.pixel_clean_sessions.is_empty() && self.pixel_clean_preparation.is_none(),
            "Stop pixel cleaning before uploading a startup image"
        );
        ensure!(
            !self.media_preparation.is_busy(),
            "Wait for LCD media preparation to finish"
        );
        if let Some(mac) = identity
            .strip_prefix("wireless:")
            .and_then(super::parse_mac_str)
        {
            return self.start_wireless_startup_image(id, identity, mac, jpeg, cancel, permit);
        }
        let receiver = self.flex_startup_receiver(&identity)?;
        let fan_devices = self.registry.fan_devices.clone();
        let mut targets = self
            .targets
            .try_lock_for(Duration::from_millis(100))
            .context("LCD targets are busy")?;
        let index = targets.iter().find_map(|(index, target)| (target.device_identity == identity).then_some(*index))
            .context("Configure a live image for this LCD in LCD settings before uploading its startup image")?;
        let screen = targets[&index].screen;
        ensure!(
            (screen.width, screen.height) == (caps.width, caps.height),
            "LCD identity or dimensions changed; refresh devices"
        );
        let (send, receive) = std::sync::mpsc::sync_channel::<ActiveTarget>(1);
        let worker_cancel = cancel.clone();
        let state = self.ipc.state.clone();
        let journal = journal_path(&self.config_path);
        let mut quarantined = self.startup_image_quarantine.clone();
        quarantined.insert(identity.clone());
        let worker = std::thread::Builder::new()
            .name("startup-image".into())
            .spawn(move || {
                let mut target = receive.recv().expect("startup target handoff");
                let mut stopped = false;
                let transfer = Arc::new(lianli_devices::startup_image::Transfer::default());
                let receiver_transfer = lianli_devices::startup_image::Transfer::default();
                let result = (|| {
                    lianli_devices::startup_image::ensure_not_cancelled(&worker_cancel)?;
                    let prepared = lianli_media::startup_image::prepare(&jpeg, caps)?;
                    target.lcd.startup_image_ready()?;
                    crate::persistence::write_json(&journal, &quarantined)
                        .context("Recording startup upload before touching device storage")?;
                    lianli_devices::startup_image::ensure_not_cancelled(&worker_cancel)?;
                    target.stop();
                    stopped = true;
                    lianli_devices::startup_image::ensure_not_cancelled(&worker_cancel)?;
                    if let Some(job) = state
                        .lock()
                        .startup_image
                        .as_mut()
                        .filter(|job| job.id == id)
                    {
                        job.status = StartupImageState::Transferring;
                    }
                    if let Some((receiver_id, slot)) = receiver {
                        fan_devices
                            .get(&receiver_id)
                            .context("Flex receiver is no longer available")?
                            .set_lcd_startup_theme_enabled(
                                slot,
                                false,
                                &worker_cancel,
                                &receiver_transfer,
                            )
                            .context("Saving Flex startup image selection")?;
                    }
                    target
                        .lcd
                        .upload_startup_image(prepared, worker_cancel, transfer.clone())
                })();
                let recovery_required = transfer.close();
                let receiver_attempted = receiver_transfer.close();
                Outcome {
                    target: Some(target),
                    result,
                    stopped,
                    recovery_required: recovery_required || receiver_attempted,
                }
            })
            .context("Starting startup image worker")?;
        let target = targets.remove(&index).expect("selected target is locked");
        if let Err(error) = send.send(target) {
            targets.insert(index, error.0);
            let _ = worker.join();
            anyhow::bail!("Startup image worker did not accept the target");
        }
        self.startup_image_job = Some(StartupImageJob {
            id,
            identity,
            cancel,
            worker: Some(worker),
            _permit: permit,
        });
        Ok(())
    }

    fn start_wireless_startup_image(
        &mut self,
        id: u64,
        identity: String,
        mac: [u8; 6],
        jpeg: Vec<u8>,
        cancel: Arc<AtomicBool>,
        permit: Option<Arc<lianli_control::write_gate::ServiceWritePermit>>,
    ) -> Result<()> {
        use lianli_shared::device_id::DeviceFamily;
        ensure!(
            self.wireless
                .devices()
                .iter()
                .any(|d| d.mac == mac && matches!(d.device_type, 10 | 11)),
            "A bound HydroShift II Circle or Square is required"
        );
        let mut targets = self
            .targets
            .try_lock_for(Duration::from_millis(100))
            .context("LCD targets are busy")?;
        let mut matching = Vec::new();
        ensure!(
            !self
                .registry
                .cached_usb_devices
                .iter()
                .any(|d| d.vid == 0x1a86 && matches!(d.pid, 0xad20 | 0xad22)),
            "Switch H2 from Desktop to LCD mode before uploading a startup image"
        );
        for (index, target) in targets.iter() {
            if !self.registry.cached_usb_devices.iter().any(|d| {
                d.device_id == target.device_identity && d.family == DeviceFamily::HydroShift2Lcd
            }) {
                continue;
            }
            let linked_mac = self
                .registry
                .fan_devices
                .get(&target.device_identity)
                .and_then(|device| device.wireless_link_mac());
            matching.push((*index, linked_mac));
        }
        let matching = wireless_target_index(mac, matching)?;
        let (send, receive) = std::sync::mpsc::sync_channel::<Option<ActiveTarget>>(1);
        let wireless = self.wireless.clone();
        let state = self.ipc.state.clone();
        let worker_cancel = cancel.clone();
        let worker = std::thread::Builder::new()
            .name("h2-wireless-image".into())
            .spawn(move || {
                let mut target = receive.recv().expect("H2 target handoff");
                let mut stopped = false;
                let result = (|| {
                    let caps =
                        lianli_shared::startup_image::capabilities(DeviceFamily::WirelessAio)
                            .context("H2 wireless image capability is unavailable")?;
                    let image = lianli_media::startup_image::prepare(&jpeg, caps)?;
                    lianli_devices::startup_image::ensure_not_cancelled(&worker_cancel)?;
                    if let Some(target) = target.as_mut() {
                        target.stop();
                        stopped = true;
                        target.lcd.pause_for_wireless_image()?;
                    }
                    lianli_devices::startup_image::ensure_not_cancelled(&worker_cancel)?;
                    let sent_at = std::time::Instant::now();
                    let sequence = wireless.switch_to_wireless_theme(&mac)?;
                    while !wireless.wireless_theme_acked(&mac, sequence, sent_at) {
                        lianli_devices::startup_image::ensure_not_cancelled(&worker_cancel)?;
                        ensure!(
                            sent_at.elapsed() < Duration::from_secs(3),
                            "H2 did not confirm wireless display mode"
                        );
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    std::thread::sleep(Duration::from_millis(200));
                    if let Some(job) = state
                        .lock()
                        .startup_image
                        .as_mut()
                        .filter(|job| job.id == id)
                    {
                        job.status = StartupImageState::Transferring;
                    }
                    wireless.send_aio_pic(&mac, &image, &worker_cancel)?;
                    Ok(false)
                })();
                Outcome {
                    target,
                    result,
                    stopped,
                    recovery_required: false,
                }
            })
            .context("Starting H2 wireless image worker")?;
        let target = matching.and_then(|index| targets.remove(&index));
        if let Err(error) = send.send(target) {
            if let Some(target) = error.0 {
                targets.insert(target.index, target);
            }
            let _ = worker.join();
            anyhow::bail!("H2 wireless image worker did not accept the target");
        }
        self.startup_image_job = Some(StartupImageJob {
            id,
            identity,
            cancel,
            worker: Some(worker),
            _permit: permit,
        });
        Ok(())
    }

    pub(super) fn poll_startup_image(&mut self) {
        if !self.startup_image_job.as_ref().is_some_and(|job| {
            job.worker
                .as_ref()
                .is_some_and(|worker| worker.is_finished())
        }) {
            return;
        }
        let mut job = self.startup_image_job.take().expect("finished job");
        let result = job.worker.take().expect("owned worker").join();
        let status = match result {
            Ok(outcome) => {
                let status = match &outcome.result {
                    Ok(response_received) => StartupImageState::Transferred {
                        response_received: *response_received,
                    },
                    Err(_) if !outcome.recovery_required && job.cancel.load(Ordering::Acquire) => {
                        StartupImageState::Cancelled
                    }
                    Err(error) => StartupImageState::Failed {
                        message: if outcome.recovery_required {
                            format!("{error:#}. {RECOVERY_MESSAGE}")
                        } else {
                            format!("{error:#}")
                        },
                    },
                };
                if let Some(mut target) = outcome.target {
                    if outcome.recovery_required && outcome.result.is_err() {
                        self.startup_image_quarantine.insert(job.identity.clone());
                        target.stop();
                    } else {
                        if outcome.stopped {
                            target.swap_media(
                                target.asset.clone(),
                                target.custom_h264,
                                self.tx.clone(),
                            );
                        }
                        self.targets.lock().insert(target.index, target);
                    }
                }
                status
            }
            Err(_) => {
                self.startup_image_quarantine.insert(job.identity.clone());
                StartupImageState::Failed {
                    message: format!("Startup upload worker failed. {RECOVERY_MESSAGE}"),
                }
            }
        };
        if let Err(error) = crate::persistence::write_json(
            &journal_path(&self.config_path),
            &self.startup_image_quarantine,
        ) {
            tracing::warn!(%error, "Startup upload recovery journal retained; reconnect may be required after restart");
        }
        if let StartupImageState::Failed { message } = &status {
            tracing::warn!(device_id = %job.identity, error = %message, "Startup image upload failed");
        }
        self.finish_startup_status(job.id, status);
        if std::mem::take(&mut self.startup_config_pending) {
            if let Some(tx) = &self.tx {
                let _ = tx.send(super::DaemonEvent::IpcUpdate);
            }
        }
    }
}

fn wireless_target_index(
    mac: [u8; 6],
    candidates: impl IntoIterator<Item = (usize, Option<[u8; 6]>)>,
) -> Result<Option<usize>> {
    let mut matching = None;
    for (index, linked_mac) in candidates {
        let linked_mac = linked_mac.context(
            "H2 USB-to-wireless identity is not available yet. Wait for device telemetry and retry",
        )?;
        if linked_mac == mac {
            ensure!(
                matching.is_none(),
                "Multiple USB displays report the same H2 wireless identity"
            );
            matching = Some(index);
        }
    }
    Ok(matching)
}

fn flex_receiver_slot(
    lcd_pid: u16,
    lcd: &(u8, Vec<u8>),
    receiver_pid: u16,
    receiver: &(u8, Vec<u8>),
) -> Option<u8> {
    if !matches!((lcd_pid, receiver_pid), (0xa018, 0x0102) | (0xa019, 0x0104))
        || lcd.0 != receiver.0
    {
        return None;
    }
    let (port, parent) = lcd.1.split_last()?;
    let (_, receiver_parent) = receiver.1.split_last()?;
    if parent != receiver_parent || !(1..=4).contains(port) {
        return None;
    }
    Some(port - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h2_upload_selects_only_the_usb_display_with_the_receiver_mac() {
        assert_eq!(
            wireless_target_index([2; 6], [(0, Some([1; 6])), (3, Some([2; 6]))]).unwrap(),
            Some(3)
        );
        assert_eq!(
            wireless_target_index([2; 6], [(0, Some([1; 6]))]).unwrap(),
            None
        );
        assert_eq!(wireless_target_index([2; 6], []).unwrap(), None);
        assert!(wireless_target_index([2; 6], [(0, None)]).is_err());
        assert!(wireless_target_index([2; 6], [(0, Some([2; 6])), (3, Some([2; 6]))]).is_err());
    }

    #[test]
    fn flex_startup_routing_requires_the_matching_model_and_usb_parent() {
        let lcd = (3, vec![2, 7, 3]);
        let receiver = (3, vec![2, 7, 5]);
        assert_eq!(flex_receiver_slot(0xa018, &lcd, 0x0102, &receiver), Some(2));
        assert_eq!(flex_receiver_slot(0xa019, &lcd, 0x0104, &receiver), Some(2));
        assert_eq!(flex_receiver_slot(0xa018, &lcd, 0x0104, &receiver), None);
        assert_eq!(flex_receiver_slot(0xa018, &lcd, 0x0101, &receiver), None);
        assert_eq!(
            flex_receiver_slot(0xa018, &lcd, 0x0102, &(4, receiver.1.clone())),
            None
        );
        assert_eq!(
            flex_receiver_slot(0xa018, &lcd, 0x0102, &(3, vec![2, 8, 5])),
            None
        );
        assert_eq!(
            flex_receiver_slot(0xa018, &(3, vec![2, 7, 5]), 0x0102, &receiver),
            None
        );
        assert_eq!(
            flex_receiver_slot(0xa018, &(3, vec![]), 0x0102, &receiver),
            None
        );
    }

    #[test]
    fn quarantine_requires_sustained_absence_and_persists_clearance() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.json");
        let present = std::collections::HashSet::from(["lcd".to_string()]);
        crate::persistence::write_json(&journal_path(&config), &present).unwrap();
        let mut service = ServiceManager::new(
            config.clone(),
            directory.path().join("socket"),
            lianli_shared::daemon::DaemonMode::User,
        )
        .unwrap();
        service.startup_image_quarantine = present.clone();
        service.reconcile_startup_quarantine(&Default::default());
        assert_eq!(service.startup_image_quarantine, present);
        service.reconcile_startup_quarantine(&present);
        assert!(service.startup_absent_since.is_empty());
        service.startup_absent_since.insert(
            "lcd".into(),
            std::time::Instant::now() - Duration::from_secs(11),
        );
        service.reconcile_startup_quarantine(&Default::default());
        assert!(service.startup_image_quarantine.is_empty());
        assert!(load_quarantine(&config).unwrap().is_empty());
    }

    #[test]
    fn quarantine_clears_when_device_returns_after_observed_absence() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.json");
        let present = std::collections::HashSet::from(["hid:lcd".to_string()]);
        crate::persistence::write_json(&journal_path(&config), &present).unwrap();
        let mut service = ServiceManager::new(
            config.clone(),
            directory.path().join("socket"),
            lianli_shared::daemon::DaemonMode::User,
        )
        .unwrap();
        service.startup_image_quarantine = present.clone();
        service.reconcile_startup_quarantine(&present);
        assert_eq!(service.startup_image_quarantine, present);
        service.reconcile_startup_quarantine(&Default::default());
        service.startup_absent_since.insert(
            "hid:lcd".into(),
            std::time::Instant::now() - Duration::from_secs(11),
        );
        service.reconcile_startup_quarantine(&present);
        assert!(service.startup_image_quarantine.is_empty());
        assert!(load_quarantine(&config).unwrap().is_empty());
    }

    #[test]
    fn quarantined_media_reports_recovery_instead_of_waiting_for_device() {
        let directory = tempfile::tempdir().unwrap();
        let mut service = ServiceManager::new(
            directory.path().join("config.json"),
            directory.path().join("socket"),
            lianli_shared::daemon::DaemonMode::User,
        )
        .unwrap();
        service.startup_image_quarantine.insert("hid:lcd".into());
        service.config = Some(
            serde_json::from_value(serde_json::json!({
                "lcds": [{ "type": "color", "serial": "hid:lcd", "rgb": [0, 0, 0] }]
            }))
            .unwrap(),
        );
        let (tx, _rx) = std::sync::mpsc::channel();
        service.prepare_media_assets(tx);
        let state = service.ipc.state.lock();
        let status = &state.telemetry.media_preparation[&0];
        assert_eq!(
            status.state,
            lianli_shared::ipc::MediaPreparationState::Failed
        );
        assert_eq!(status.error.as_deref(), Some(RECOVERY_MESSAGE));
    }

    #[test]
    fn daemon_restart_clears_previous_recovery_without_reuploading() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.json");
        assert!(load_quarantine(&config).unwrap().is_empty());
        let devices = std::collections::HashSet::from(["lcd".to_string()]);
        crate::persistence::write_json(&journal_path(&config), &devices).unwrap();
        assert_eq!(load_quarantine(&config).unwrap(), devices);
        let service = ServiceManager::new(
            config.clone(),
            directory.path().join("socket"),
            lianli_shared::daemon::DaemonMode::User,
        )
        .unwrap();
        assert!(service.startup_image_quarantine.is_empty());
        assert!(service.startup_image_job.is_none());
        assert!(load_quarantine(&config).unwrap().is_empty());
    }

    #[test]
    fn manual_recovery_clears_only_the_selected_lcd() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.json");
        let mut service = ServiceManager::new(
            config.clone(),
            directory.path().join("socket"),
            lianli_shared::daemon::DaemonMode::User,
        )
        .unwrap();
        service
            .startup_image_quarantine
            .extend(["hid:a".into(), "hid:b".into()]);
        service.clear_startup_recovery("serial:hid:a").unwrap();
        assert_eq!(
            service.startup_image_quarantine,
            std::collections::HashSet::from(["hid:b".into()])
        );
        assert_eq!(
            load_quarantine(&config).unwrap(),
            service.startup_image_quarantine
        );
        assert!(service.media_reload_pending);
        assert!(service.startup_image_job.is_none());
        assert!(service.clear_startup_recovery("serial:hid:a").is_err());
    }

    #[test]
    fn upload_preserves_applied_cooling_and_pending_configuration() {
        let directory = tempfile::tempdir().unwrap();
        let mut service = ServiceManager::new(
            directory.path().join("config.json"),
            directory.path().join("daemon.sock"),
            lianli_shared::daemon::DaemonMode::User,
        )
        .unwrap();
        let old = lianli_shared::config::AppConfig::default();
        let mut pending = old.clone();
        pending.aio.insert(
            "h2".into(),
            lianli_shared::aio::AioConfig::defaults_for_host(),
        );
        service.config = Some(old);
        service.ipc.state.lock().config = Some(pending);
        service.startup_image_job = Some(StartupImageJob {
            id: 1,
            identity: "h2".into(),
            cancel: Arc::new(AtomicBool::new(false)),
            worker: None,
            _permit: None,
        });
        service.startup_image_quarantine.insert("h2".into());
        assert!(service.clear_startup_recovery("h2").is_err());
        assert!(service.startup_image_quarantine.contains("h2"));
        service.ensure_aio_defaults();
        service.sync_ipc_state();
        assert!(service.startup_config_pending);
        assert!(!service.config.as_ref().unwrap().aio.contains_key("h2"));
        assert!(service
            .ipc
            .state
            .lock()
            .config
            .as_ref()
            .unwrap()
            .aio
            .contains_key("h2"));
        assert!(service
            .handle_set_ene6k77_fan_quantity("unavailable", 1)
            .unwrap_err()
            .to_string()
            .contains("startup image"));
    }
}
