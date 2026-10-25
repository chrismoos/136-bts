use serde::Deserialize;

const DEFAULT_TX_PRIORITY: u8 = 80;
const DEFAULT_RX_PRIORITY: u8 = 75;
const MIN_PRIORITY: u8 = 1;
const MAX_PRIORITY: u8 = 99;

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub enabled: bool,
    pub tx_priority: u8,
    pub rx_priority: u8,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            tx_priority: DEFAULT_TX_PRIORITY,
            rx_priority: DEFAULT_RX_PRIORITY,
        }
    }
}

impl Config {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        for (value, field) in [
            (self.tx_priority, "realtime.tx_priority"),
            (self.rx_priority, "realtime.rx_priority"),
        ] {
            if !(MIN_PRIORITY..=MAX_PRIORITY).contains(&value) {
                return Err(field);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Role {
    Tx,
    Rx,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use platform::Guard;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) struct Guard;

pub(crate) fn apply(config: Config, role: Role) -> Option<Guard> {
    if !config.enabled {
        tracing::info!(?role, "real-time scheduling disabled");
        return None;
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let priority = match role {
            Role::Tx => config.tx_priority,
            Role::Rx => config.rx_priority,
        };
        match platform::apply(priority) {
            Ok(guard) => {
                #[cfg(target_os = "linux")]
                tracing::info!(
                    ?role,
                    priority,
                    policy = "SCHED_FIFO",
                    "real-time scheduling verified"
                );
                #[cfg(target_os = "macos")]
                tracing::info!(
                    ?role,
                    period_us = platform::PERIOD_US,
                    computation_us = platform::COMPUTATION_US,
                    policy = "Mach time constraint",
                    "real-time scheduling applied"
                );
                Some(guard)
            }
            Err(error) => {
                tracing::warn!(?role, %error, "real-time scheduling unavailable, retaining existing scheduling. Linux requires CAP_SYS_NICE or sufficient RLIMIT_RTPRIO");
                None
            }
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        tracing::warn!(?role, "real-time scheduling unsupported on this platform");
        None
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::{error::Error, marker::PhantomData, rc::Rc};
    use thread_priority::{
        RealtimeThreadSchedulePolicy, ThreadPriority, ThreadSchedulePolicy,
        set_thread_priority_and_policy, thread_native_id, thread_schedule_policy_param,
    };

    pub(crate) struct Guard {
        policy: ThreadSchedulePolicy,
        priority: ThreadPriority,
        thread: PhantomData<Rc<()>>,
    }

    pub(super) fn apply(priority: u8) -> Result<Guard, Box<dyn Error>> {
        let thread = thread_native_id();
        let (policy, params) = thread_schedule_policy_param(thread)?;
        if policy == ThreadSchedulePolicy::Realtime(RealtimeThreadSchedulePolicy::Deadline) {
            return Err("retaining existing SCHED_DEADLINE scheduling".into());
        }
        // Restoring a normal policy must preserve niceness as well as static priority.
        let saved = if matches!(policy, ThreadSchedulePolicy::Normal(_)) {
            let nice = rustix::process::getpriority_process(None)?;
            (0..=super::MAX_PRIORITY)
                .filter_map(|value| Some(ThreadPriority::Crossplatform(value.try_into().ok()?)))
                .find(|candidate| candidate.to_posix(policy) == Ok(nice))
                .ok_or("could not preserve current thread niceness")?
        } else {
            ThreadPriority::from_posix(params)
        };
        let requested = ThreadSchedulePolicy::Realtime(RealtimeThreadSchedulePolicy::Fifo);
        set_thread_priority_and_policy(
            thread,
            ThreadPriority::Crossplatform(priority.try_into()?),
            requested,
        )?;
        let guard = Guard {
            policy,
            priority: saved,
            thread: PhantomData,
        };
        let (actual_policy, actual) = thread_schedule_policy_param(thread)?;
        if actual_policy != requested || actual.sched_priority != i32::from(priority) {
            return Err("SCHED_FIFO priority verification failed".into());
        }
        Ok(guard)
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            if let Err(error) =
                set_thread_priority_and_policy(thread_native_id(), self.priority, self.policy)
            {
                tracing::warn!(%error, "could not restore thread scheduling");
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use audio_thread_priority::{
        RtPriorityHandle, demote_current_thread_from_real_time, promote_current_thread_to_real_time,
    };
    use std::{error::Error, marker::PhantomData, rc::Rc};

    const PERIOD_FRAMES: u32 = 2;
    const SAMPLE_RATE_HZ: u32 = 1000;
    pub(super) const PERIOD_US: u32 = PERIOD_FRAMES * 1_000_000 / SAMPLE_RATE_HZ;
    pub(super) const COMPUTATION_US: u32 = PERIOD_US / 2;

    pub(crate) struct Guard {
        handle: Option<RtPriorityHandle>,
        thread: PhantomData<Rc<()>>,
    }

    pub(super) fn apply(_priority: u8) -> Result<Guard, Box<dyn Error>> {
        let handle = promote_current_thread_to_real_time(PERIOD_FRAMES, SAMPLE_RATE_HZ)?;
        Ok(Guard {
            handle: Some(handle),
            thread: PhantomData,
        })
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            if let Some(handle) = self.handle.take()
                && let Err(error) = demote_current_thread_from_real_time(handle)
            {
                tracing::warn!(%error, "could not restore thread scheduling");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_defaults_and_priority_limits() {
        let config: Config = serde_yaml_ng::from_str("{}").unwrap();
        assert!(config.enabled);
        assert_eq!((config.tx_priority, config.rx_priority), (80, 75));
        for field in ["tx_priority", "rx_priority"] {
            for value in [0, 100] {
                let config: Config = serde_yaml_ng::from_str(&format!("{field}: {value}")).unwrap();
                assert!(config.validate().is_err());
            }
            for value in [1, 99] {
                let config: Config = serde_yaml_ng::from_str(&format!("{field}: {value}")).unwrap();
                assert!(config.validate().is_ok());
            }
        }
        let config: Config = serde_yaml_ng::from_str("enabled: false").unwrap();
        assert!(apply(config, Role::Tx).is_none());
        assert!(apply(config, Role::Rx).is_none());
        assert!(serde_yaml_ng::from_str::<Config>("tx_priorty: 80").is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn scheduling_is_verified_and_restored_on_the_calling_thread() {
        std::thread::spawn(|| {
            use thread_priority::{
                RealtimeThreadSchedulePolicy, ThreadSchedulePolicy, thread_native_id,
                thread_schedule_policy_param,
            };
            let before = thread_schedule_policy_param(thread_native_id()).unwrap();
            let nice = rustix::process::getpriority_process(None).unwrap();
            let scheduling = apply(Config::default(), Role::Tx);
            if scheduling.is_some() {
                let (policy, params) = thread_schedule_policy_param(thread_native_id()).unwrap();
                assert_eq!(
                    policy,
                    ThreadSchedulePolicy::Realtime(RealtimeThreadSchedulePolicy::Fifo)
                );
                assert_eq!(params.sched_priority, i32::from(DEFAULT_TX_PRIORITY));
            }
            drop(scheduling);
            assert_eq!(
                thread_schedule_policy_param(thread_native_id()).unwrap(),
                before
            );
            assert_eq!(rustix::process::getpriority_process(None).unwrap(), nice);
        })
        .join()
        .unwrap();
    }
}
