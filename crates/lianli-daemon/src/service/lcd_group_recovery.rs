use super::{runtime::ActiveTarget, ServiceManager};
use anyhow::{Context, Result};
use lianli_devices::wireless::WirelessFanType;
use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tracing::{info, warn};

const DISCOVERY_GRACE: Duration = Duration::from_secs(10);
const RETRY_DELAY: Duration = Duration::from_secs(30);
const MAX_ATTEMPTS: u8 = 2;
const SETTLE_TIME: Duration = Duration::from_secs(6);

#[derive(Default)]
struct RecoveryState {
    missing_since: Option<Instant>,
    last_attempt: Option<Instant>,
    attempts: u8,
    exhausted: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum Decision {
    Wait,
    Reboot,
    Exhausted,
}

impl RecoveryState {
    fn observe(&mut self, now: Instant, missing: bool) -> Decision {
        if !missing {
            self.missing_since = None;
            return Decision::Wait;
        }
        let since = *self.missing_since.get_or_insert(now);
        if now.duration_since(since) < DISCOVERY_GRACE
            || self
                .last_attempt
                .is_some_and(|last| now.duration_since(last) < RETRY_DELAY)
        {
            return Decision::Wait;
        }
        if self.attempts < MAX_ATTEMPTS {
            Decision::Reboot
        } else if !self.exhausted {
            self.exhausted = true;
            Decision::Exhausted
        } else {
            Decision::Wait
        }
    }

    fn attempted(&mut self, now: Instant) {
        self.attempts += 1;
        self.last_attempt = Some(now);
    }
}

type UsbLocation = (u8, Vec<u8>);

fn same_hub(first: &UsbLocation, second: &UsbLocation) -> bool {
    first.0 == second.0
        && first.1.len() > 1
        && second.1.len() > 1
        && first.1[..first.1.len() - 1] == second.1[..second.1.len() - 1]
}

#[derive(Clone)]
enum Owner {
    Wired(String),
    Wireless([u8; 6]),
}

struct Group {
    id: String,
    owner: Owner,
    location: UsbLocation,
    lcd_pid: u16,
    expected: u8,
    members: HashSet<String>,
}

impl Group {
    fn owns_target(&self, identity: &str) -> bool {
        let prefix = format!("hid:1cbe:{:04x}:{}-", self.lcd_pid, self.location.0);
        let Some(path) = identity.strip_prefix(&prefix) else {
            return false;
        };
        let Some(ports) = path
            .split('.')
            .map(|port| port.parse::<u8>().ok())
            .collect::<Option<Vec<_>>>()
        else {
            return false;
        };
        same_hub(&self.location, &(self.location.0, ports))
    }
}

struct Job {
    id: String,
    stop: Arc<AtomicBool>,
    worker: JoinHandle<Result<bool>>,
}

#[derive(Default)]
pub(super) struct LcdGroupRecovery {
    states: HashMap<String, RecoveryState>,
    job: Option<Job>,
    settle_until: Option<Instant>,
}

impl LcdGroupRecovery {
    pub(super) fn busy(&self) -> bool {
        self.job.is_some() || self.settle_until.is_some()
    }

    pub(super) fn poll(&mut self) {
        if self
            .job
            .as_ref()
            .is_some_and(|job| job.worker.is_finished())
        {
            self.finish_job(false);
            self.settle_until = Some(Instant::now() + SETTLE_TIME);
        }
        if self
            .settle_until
            .is_some_and(|until| Instant::now() >= until)
        {
            self.settle_until = None;
        }
    }

    fn finish_job(&mut self, cancel: bool) {
        let Some(job) = self.job.take() else { return };
        if cancel {
            job.stop.store(true, Ordering::Release);
        }
        match job.worker.join() {
            Ok(Ok(true)) => {
                info!(device_id = %job.id, "LCD group reboot sent; waiting for USB rediscovery")
            }
            Ok(Ok(false)) => {}
            Ok(Err(error)) if !cancel => {
                warn!(device_id = %job.id, %error, "LCD group recovery failed")
            }
            Ok(Err(_)) => {}
            Err(_) => warn!(device_id = %job.id, "LCD group recovery worker panicked"),
        }
    }

    pub(super) fn stop(&mut self) {
        self.finish_job(true);
        self.settle_until = None;
    }
}

impl Drop for LcdGroupRecovery {
    fn drop(&mut self) {
        self.stop();
    }
}

impl ServiceManager {
    pub(super) fn recover_lcd_groups(&mut self) {
        self.lcd_group_recovery.poll();
        if self.lcd_group_recovery.busy()
            || self.startup_image_job.is_some()
            || !self.startup_image_quarantine.is_empty()
            || self.display_switch.is_some()
            || self.pixel_clean_preparation.is_some()
            || !self.pixel_clean_sessions.is_empty()
            || lianli_transport::usb::shutting_down()
        {
            return;
        }
        let wireless = self.wireless.devices();
        let mut present: HashSet<_> = self
            .registry
            .cached_usb_devices
            .iter()
            .map(|d| d.device_id.clone())
            .collect();
        present.extend(
            wireless
                .iter()
                .map(|device| format!("wireless:{}", device.mac_str())),
        );
        self.lcd_group_recovery
            .states
            .retain(|id, _| present.contains(id));
        let groups = self.lcd_recovery_groups(&wireless);
        let now = Instant::now();
        for group in groups {
            let state = self
                .lcd_group_recovery
                .states
                .entry(group.id.clone())
                .or_default();
            match state.observe(now, group.members.len() < usize::from(group.expected)) {
                Decision::Wait => continue,
                Decision::Exhausted => {
                    warn!(device_id = %group.id, "LCD group is still incomplete after two recovery attempts; check USB/power connections and power-cycle the group");
                    continue;
                }
                Decision::Reboot => {}
            }
            let Some(mut targets) = self.targets.try_lock_for(Duration::from_millis(50)) else {
                continue;
            };
            let indices: Vec<_> = targets
                .iter()
                .filter(|(_, target)| group.owns_target(&target.device_identity))
                .map(|(index, _)| *index)
                .collect();
            let retired: Vec<ActiveTarget> = indices
                .into_iter()
                .filter_map(|index| targets.remove(&index))
                .collect();
            drop(targets);
            state.attempted(now);
            let stop = Arc::new(AtomicBool::new(false));
            let worker_stop = Arc::clone(&stop);
            let devices = Arc::clone(&self.registry.fan_devices);
            let radio = self.wireless.clone();
            let hid_backend = self.hid_backend();
            let id = group.id.clone();
            let worker = std::thread::Builder::new()
                .name("lcd-group-recovery".into())
                .spawn(move || {
                    let mut retired = retired;
                    let mut builder = lianli_devices::crypto::PacketBuilder::new();
                    let mut teardown_error = None;
                    for target in &mut retired {
                        if let Err(error) = target.shutdown(Some(&radio), &mut builder, true) {
                            if !device_disconnected(&error) {
                                teardown_error.get_or_insert(error);
                            }
                        }
                    }
                    drop(retired);
                    if let Some(error) = teardown_error {
                        return Err(error.context("Stopping LCD playback before recovery"));
                    }
                    if worker_stop.load(Ordering::Acquire) || lianli_transport::usb::shutting_down()
                    {
                        return Ok(false);
                    }
                    if !group_still_missing(&group, hid_backend)? {
                        return Ok(false);
                    }
                    if worker_stop.load(Ordering::Acquire) || lianli_transport::usb::shutting_down()
                    {
                        return Ok(false);
                    }
                    match &group.owner {
                        Owner::Wired(id) => devices
                            .get(id)
                            .context("LCD receiver disconnected")?
                            .reboot_lcd_group(&worker_stop)?,
                        Owner::Wireless(mac) => radio.reboot_lcd_group_once(mac)?,
                    }
                    Ok(true)
                });
            match worker {
                Ok(worker) => self.lcd_group_recovery.job = Some(Job { id, stop, worker }),
                Err(error) => warn!(device_id = %id, %error, "Cannot start LCD group recovery"),
            }
            break;
        }
    }

    fn lcd_recovery_groups(
        &self,
        wireless: &[lianli_devices::wireless::DiscoveredDevice],
    ) -> Vec<Group> {
        let mut groups = Vec::new();
        let mut covered_macs = HashSet::new();
        for receiver in &self.registry.cached_usb_devices {
            let lcd_pid = match (receiver.vid, receiver.pid) {
                (0x43a8, 0x0102) => 0xa018,
                (0x43a8, 0x0104) => 0xa019,
                _ => continue,
            };
            let Some(device) = self.registry.fan_devices.get(&receiver.device_id) else {
                continue;
            };
            let Some(expected) = device.lcd_group_size().filter(|count| *count > 0) else {
                continue;
            };
            let Some(location) = self.registry.usb_locations.get(&receiver.device_id) else {
                continue;
            };
            if let Some(mac) = device.wireless_link_mac() {
                covered_macs.insert(mac);
            }
            groups.push(Group {
                id: receiver.device_id.clone(),
                owner: Owner::Wired(receiver.device_id.clone()),
                location: location.clone(),
                lcd_pid,
                expected,
                members: HashSet::new(),
            });
        }
        for device in wireless {
            if covered_macs.contains(&device.mac) {
                continue;
            }
            let Some(expected) = device.lcd_group_size().filter(|count| *count > 0) else {
                continue;
            };
            let lcd_pid = match device.fan_type {
                WirelessFanType::Slv3Led | WirelessFanType::Slv3Lcd => 0x0005,
                WirelessFanType::Tlv2Led | WirelessFanType::Tlv2Lcd => 0x0006,
                WirelessFanType::TlV3 { .. } => 0xa018,
                WirelessFanType::SlInfV3 { .. } => 0xa019,
                _ => continue,
            };
            let mut mappings = self
                .registry
                .v2_hid_entries
                .iter()
                .filter(|entry| entry.mac == device.mac);
            let Some(mapping) = mappings.next() else {
                continue;
            };
            if mappings.next().is_some() {
                continue;
            }
            groups.push(Group {
                id: format!("wireless:{}", device.mac_str()),
                owner: Owner::Wireless(device.mac),
                location: (mapping.bus, mapping.port_numbers.clone()),
                lcd_pid,
                expected,
                members: HashSet::new(),
            });
        }
        let ambiguous: HashSet<_> = groups
            .iter()
            .filter(|group| {
                groups
                    .iter()
                    .any(|other| group.id != other.id && same_hub(&group.location, &other.location))
            })
            .map(|group| group.id.clone())
            .collect();
        groups.retain(|group| !ambiguous.contains(&group.id) && group.location.1.len() > 1);
        groups.retain_mut(|group| {
            for lcd in self
                .registry
                .cached_usb_devices
                .iter()
                .filter(|lcd| lcd.vid == 0x1cbe && lcd.pid == group.lcd_pid)
            {
                let Some(location) = self.registry.usb_locations.get(&lcd.device_id) else {
                    return false;
                };
                if same_hub(&group.location, location) {
                    group.members.insert(lcd.device_id.clone());
                }
            }
            true
        });
        groups
    }
}

fn device_disconnected(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<rusb::Error>() == Some(&rusb::Error::NoDevice)
            || matches!(
                cause.downcast_ref::<lianli_transport::TransportError>(),
                Some(lianli_transport::TransportError::Usb(rusb::Error::NoDevice))
            )
    })
}

fn group_still_missing(
    group: &Group,
    hid_backend: lianli_shared::config::HidBackend,
) -> Result<bool> {
    if let Owner::Wireless(mac) = &group.owner {
        let entries = lianli_devices::wireless::query_v2_hid_macs(hid_backend);
        let matches: Vec<_> = entries.iter().filter(|entry| entry.mac == *mac).collect();
        if matches.len() != 1
            || (matches[0].bus, &matches[0].port_numbers) != (group.location.0, &group.location.1)
        {
            return Ok(false);
        }
    }
    let mut present = 0;
    let mut anchor_present = false;
    for device in rusb::devices()?.iter() {
        let descriptor = device.device_descriptor()?;
        let relevant = (descriptor.vendor_id() == 0x1cbe
            && descriptor.product_id() == group.lcd_pid)
            || matches!(
                (descriptor.vendor_id(), descriptor.product_id()),
                (0x43a8, 0x0102 | 0x0104) | (0x1a86, 0x2107)
            );
        if !relevant {
            continue;
        }
        let location = (device.bus_number(), device.port_numbers()?);
        if descriptor.vendor_id() != 0x1cbe
            && location != group.location
            && same_hub(&group.location, &location)
        {
            return Ok(false);
        }
        if location == group.location && descriptor.vendor_id() != 0x1cbe {
            anchor_present = true;
        }
        if descriptor.vendor_id() == 0x1cbe
            && descriptor.product_id() == group.lcd_pid
            && same_hub(&group.location, &location)
        {
            present += 1;
        }
    }
    Ok(anchor_present && present < group.expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnected_screen_does_not_prevent_group_recovery() {
        let missing =
            anyhow::Error::from(lianli_transport::TransportError::Usb(rusb::Error::NoDevice))
                .context("LCD teardown");
        assert!(device_disconnected(&missing));
        assert!(device_disconnected(&anyhow::Error::from(
            rusb::Error::NoDevice
        )));
        assert!(!device_disconnected(&anyhow::Error::from(
            rusb::Error::Timeout
        )));
        assert!(!device_disconnected(&anyhow::anyhow!(
            "LCD sender did not finish shutdown"
        )));
    }

    #[test]
    fn missing_group_gets_grace_and_only_one_delayed_retry() {
        let now = Instant::now();
        let mut state = RecoveryState::default();
        assert_eq!(state.observe(now, true), Decision::Wait);
        assert_eq!(
            state.observe(now + DISCOVERY_GRACE - Duration::from_millis(1), true),
            Decision::Wait
        );
        let first = now + DISCOVERY_GRACE;
        assert_eq!(state.observe(first, true), Decision::Reboot);
        state.attempted(first);
        assert_eq!(
            state.observe(first + RETRY_DELAY - Duration::from_millis(1), true),
            Decision::Wait
        );
        let second = first + RETRY_DELAY;
        assert_eq!(state.observe(second, true), Decision::Reboot);
        state.attempted(second);
        assert_eq!(
            state.observe(second + RETRY_DELAY, true),
            Decision::Exhausted
        );
        assert_eq!(
            state.observe(second + RETRY_DELAY * 100, true),
            Decision::Wait
        );
    }

    #[test]
    fn healthy_scan_restarts_grace_without_replenishing_attempts() {
        let now = Instant::now();
        let mut state = RecoveryState::default();
        state.observe(now, true);
        state.attempted(now + DISCOVERY_GRACE);
        state.observe(now + DISCOVERY_GRACE + Duration::from_secs(1), false);
        let later = now + RETRY_DELAY * 2;
        assert_eq!(state.observe(later, true), Decision::Wait);
        assert_eq!(
            state.observe(later + DISCOVERY_GRACE, true),
            Decision::Reboot
        );
        state.attempted(later + DISCOVERY_GRACE);
        assert_eq!(
            state.observe(later + DISCOVERY_GRACE + RETRY_DELAY, true),
            Decision::Exhausted
        );
    }

    #[test]
    fn matching_hubs_require_bus_and_parent_path() {
        assert!(same_hub(&(3, vec![2, 7, 5]), &(3, vec![2, 7, 1])));
        assert!(!same_hub(&(3, vec![2, 7, 5]), &(3, vec![2, 8, 1])));
        assert!(!same_hub(&(3, vec![2, 7, 5]), &(4, vec![2, 7, 1])));
        assert!(!same_hub(&(3, vec![]), &(3, vec![])));
        assert!(!same_hub(&(3, vec![1]), &(3, vec![2])));
    }

    #[test]
    fn recovery_stops_only_sibling_screens_including_previously_missing_targets() {
        let group = Group {
            id: "receiver".into(),
            owner: Owner::Wired("receiver".into()),
            location: (3, vec![2, 7, 5]),
            lcd_pid: 0xa018,
            expected: 3,
            members: HashSet::new(),
        };
        assert!(group.owns_target("hid:1cbe:a018:3-2.7.1"));
        assert!(group.owns_target("hid:1cbe:a018:3-2.7.3"));
        for unrelated in [
            "hid:1cbe:a018:4-2.7.1",
            "hid:1cbe:a018:3-2.8.1",
            "hid:1cbe:a018:3-2.7.1.2",
            "hid:1cbe:a019:3-2.7.1",
            "hid:1cbe:a018:3-2.7.invalid",
            "hid:serial",
        ] {
            assert!(!group.owns_target(unrelated), "{unrelated}");
        }
    }

    #[test]
    fn shutdown_cancels_and_joins_recovery_before_releasing_state() {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker = std::thread::spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                std::thread::park_timeout(Duration::from_millis(1));
            }
            Ok(false)
        });
        let mut recovery = LcdGroupRecovery {
            job: Some(Job {
                id: "receiver".into(),
                stop: stop.clone(),
                worker,
            }),
            settle_until: Some(Instant::now() + SETTLE_TIME),
            states: HashMap::new(),
        };
        recovery.stop();
        assert!(stop.load(Ordering::Acquire));
        assert!(!recovery.busy());
    }
}
