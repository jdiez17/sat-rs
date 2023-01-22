use crate::pool::StoreAddr;
use alloc::collections::btree_map::{Entry, Range};
use core::time::Duration;
use spacepackets::time::UnixTimestamp;
use std::collections::BTreeMap;
use std::time::SystemTimeError;
use std::vec;
use std::vec::Vec;

#[derive(Debug)]
pub struct PusScheduler {
    tc_map: BTreeMap<UnixTimestamp, Vec<StoreAddr>>,
    current_time: UnixTimestamp,
    time_margin: Duration,
    enabled: bool,
}

impl PusScheduler {
    pub fn new(init_current_time: UnixTimestamp, time_margin: Duration) -> Self {
        PusScheduler {
            tc_map: Default::default(),
            current_time: init_current_time,
            time_margin,
            enabled: true,
        }
    }

    pub fn num_scheduled_telecommands(&self) -> u64 {
        let mut num_entries = 0;
        for entries in &self.tc_map {
            num_entries += entries.1.len();
        }
        num_entries.into()
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn enable(&mut self) {
        self.enabled = true;
    }

    pub fn disable(&mut self) {
        self.enabled = false;
    }

    pub fn reset(&mut self) {
        self.enabled = false;
        self.tc_map.clear();
    }

    pub fn update_time(&mut self, current_time: UnixTimestamp) {
        self.current_time = current_time;
    }

    pub fn current_time(&self) -> &UnixTimestamp {
        &self.current_time
    }

    pub fn insert_tc(&mut self, time_stamp: UnixTimestamp, addr: StoreAddr) -> bool {
        if time_stamp > self.current_time + self.time_margin {
            return false;
        }
        match self.tc_map.entry(time_stamp) {
            Entry::Vacant(e) => e.insert(vec![addr]),
            Entry::Occupied(mut v) => v.get_mut().push(addr),
        }
        true
    }

    pub fn telecommands_to_release(&self) -> Range<'_, UnixTimestamp, Vec<StoreAddr>> {
        self.tc_map.range(..=self.current_time)
    }

    #[cfg(feature = "std")]
    #[cfg_attr(doc_cfg, doc(cfg(feature = "std")))]
    pub fn update_time_from_now(&mut self) -> Result<(), SystemTimeError> {
        self.current_time = UnixTimestamp::from_now()?;
        Ok(())
    }

    pub fn release_telecommands<R: FnMut(bool, &StoreAddr)>(&mut self, mut releaser: R) {
        let tcs_to_release = self.telecommands_to_release();
        for tc in tcs_to_release {
            for addr in tc.1 {
                releaser(self.enabled, addr);
            }
        }
        self.tc_map.retain(|k, _| k > &self.current_time);
    }
}

#[cfg(test)]
mod tests {
    use crate::pool::StoreAddr;
    use crate::pus::scheduling::PusScheduler;
    use spacepackets::time::UnixTimestamp;
    use std::time::Duration;

    #[test]
    fn basic() {
        let mut scheduler =
            PusScheduler::new(UnixTimestamp::new_only_seconds(0), Duration::from_secs(5));
        assert!(scheduler.is_enabled());
        scheduler.disable();
        assert!(!scheduler.is_enabled());
    }

    #[test]
    fn reset() {
        let mut scheduler =
            PusScheduler::new(UnixTimestamp::new_only_seconds(0), Duration::from_secs(5));
        scheduler.insert_tc(
            UnixTimestamp::new_only_seconds(200),
            StoreAddr {
                pool_idx: 0,
                packet_idx: 1,
            },
        );
        scheduler.insert_tc(
            UnixTimestamp::new_only_seconds(200),
            StoreAddr {
                pool_idx: 0,
                packet_idx: 2,
            },
        );
        scheduler.insert_tc(
            UnixTimestamp::new_only_seconds(300),
            StoreAddr {
                pool_idx: 0,
                packet_idx: 2,
            },
        );
        assert_eq!(scheduler.num_scheduled_telecommands(), 3);
        assert!(scheduler.is_enabled());
        scheduler.reset();
        assert!(!scheduler.is_enabled());
        assert_eq!(scheduler.num_scheduled_telecommands(), 0);
    }
}
