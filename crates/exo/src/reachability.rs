//! The OS's word on whether the Mac has a network. It only says the state flipped, and the
//! account then checks whether Gmail answers
//! ([`Account::network_changed`](crate::account::Account::network_changed)).

/// Watches the Mac's route to the internet while alive.
pub struct Reachability {
    /// Watching until dropped.
    #[cfg(target_os = "macos")]
    _watch: macos::Watch,
}

impl Reachability {
    /// Calls `changed` whenever the Mac gains or loses its route to the internet, and once at
    /// the start when it has none (a launch with the network off). `None` where the OS can't be
    /// asked.
    pub fn watch(changed: impl Fn() + Send + Sync + 'static) -> Option<Self> {
        #[cfg(target_os = "macos")]
        return macos::Watch::start(Box::new(changed)).map(|watch| Self { _watch: watch });
        #[cfg(not(target_os = "macos"))]
        {
            let _ = changed;
            None
        }
    }
}

/// Tracks IPv4 and IPv6 default-route reachability together: the Mac is online whenever *either*
/// protocol family has a usable route (covering IPv6-only / NAT64 networks).
// Off macOS `Reachability::watch` returns `None`, so only `tests` exercises `DualStack`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct DualStack {
    v4: bool,
    v6: bool,
    online: bool,
}

// Off macOS `Reachability::watch` returns `None`, so only `tests` exercises `DualStack`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
impl DualStack {
    fn new(v4: bool, v6: bool) -> Self {
        Self {
            v4,
            v6,
            online: v4 || v6,
        }
    }

    fn is_online(&self) -> bool {
        self.online
    }

    fn update_v4(&mut self, v4: bool) -> bool {
        self.v4 = v4;
        self.settle()
    }

    fn update_v6(&mut self, v6: bool) -> bool {
        self.v6 = v6;
        self.settle()
    }

    fn settle(&mut self) -> bool {
        let now = self.v4 || self.v6;
        if self.online != now {
            self.online = now;
            true
        } else {
            false
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    #![expect(
        deprecated,
        reason = "SCNetworkReachability is deprecated in favour of Network.framework's path monitor, which has no bindings in the objc2 family Exo uses"
    )]

    use std::ffi::c_void;
    use std::ptr::NonNull;
    use std::sync::{Mutex, PoisonError};

    use dispatch2::{DispatchQueue, DispatchRetained};
    use objc2_core_foundation::CFRetained;
    use objc2_system_configuration::{
        SCNetworkReachability, SCNetworkReachabilityContext, SCNetworkReachabilityFlags,
    };

    use super::DualStack;

    /// What the callbacks reach through their `info` pointer.
    struct State {
        changed: Box<dyn Fn() + Send + Sync>,
        stack: Mutex<DualStack>,
    }

    pub(super) struct Watch {
        v4: CFRetained<SCNetworkReachability>,
        v6: CFRetained<SCNetworkReachability>,
        queue: DispatchRetained<DispatchQueue>,
        /// Owned, and freed once the callbacks can no longer run.
        state: NonNull<State>,
    }

    /// Whether `flags` mean online, which is reachable without first having to bring a
    /// connection up.
    pub(super) fn online(flags: SCNetworkReachabilityFlags) -> bool {
        flags.contains(SCNetworkReachabilityFlags::Reachable)
            && !flags.contains(SCNetworkReachabilityFlags::ConnectionRequired)
    }

    fn probe(target: &SCNetworkReachability) -> Option<bool> {
        let mut flags = SCNetworkReachabilityFlags(0);
        // SAFETY: `flags` is valid for writes.
        let known = unsafe { target.flags(NonNull::from(&mut flags)) };
        known.then(|| online(flags))
    }

    impl Watch {
        pub(super) fn start(changed: Box<dyn Fn() + Send + Sync>) -> Option<Self> {
            // The IPv4 (`0.0.0.0`) and IPv6 (`::`) default routes.
            // SAFETY: `sockaddr_in` and `sockaddr_in6` are plain data; all zeros is the
            // unspecified/default-route address in each family.
            let mut v4_addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
            v4_addr.sin_len = std::mem::size_of::<libc::sockaddr_in>() as u8;
            v4_addr.sin_family = libc::AF_INET as u8;
            let mut v6_addr: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
            v6_addr.sin6_len = std::mem::size_of::<libc::sockaddr_in6>() as u8;
            v6_addr.sin6_family = libc::AF_INET6 as u8;
            // SAFETY: both addresses are valid socket addresses, which the calls copy.
            let v4 = unsafe {
                SCNetworkReachability::with_address(None, NonNull::from(&v4_addr).cast())
            }?;
            let v6 = unsafe {
                SCNetworkReachability::with_address(None, NonNull::from(&v6_addr).cast())
            }?;
            let (v4_online, v6_online) = match (probe(&v4), probe(&v6)) {
                (None, None) => (true, true),
                (v4_state, v6_state) => (v4_state.unwrap_or(false), v6_state.unwrap_or(false)),
            };
            let stack = DualStack::new(v4_online, v6_online);
            let now_online = stack.is_online();
            let state = NonNull::from(Box::leak(Box::new(State {
                changed,
                stack: Mutex::new(stack),
            })));
            let watch = Self {
                v4,
                v6,
                queue: DispatchQueue::new("exo.reachability", None),
                state,
            };
            let mut context = SCNetworkReachabilityContext {
                version: 0,
                info: state.as_ptr().cast(),
                retain: None,
                release: None,
                copyDescription: None,
            };
            // SAFETY: callbacks only read `State` through `info`, which outlives their
            // registration (`Drop` unregisters both targets and drains `queue` before freeing it).
            let watching = unsafe {
                watch.v4.set_callback(Some(callback_v4), &mut context)
                    && watch.v4.set_dispatch_queue(Some(&watch.queue))
                    && watch.v6.set_callback(Some(callback_v6), &mut context)
                    && watch.v6.set_dispatch_queue(Some(&watch.queue))
            };
            if !watching {
                return None;
            }
            if !now_online {
                // SAFETY: `state` is alive until `watch` drops.
                (unsafe { watch.state.as_ref() }.changed)();
            }
            Some(watch)
        }
    }

    unsafe extern "C-unwind" fn callback_v4(
        _: NonNull<SCNetworkReachability>,
        flags: SCNetworkReachabilityFlags,
        info: *mut c_void,
    ) {
        // SAFETY: `info` is the `State` `start` registered, alive while the callback is.
        let state = unsafe { &*info.cast::<State>() };
        let flipped = state
            .stack
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .update_v4(online(flags));
        if flipped {
            (state.changed)();
        }
    }

    unsafe extern "C-unwind" fn callback_v6(
        _: NonNull<SCNetworkReachability>,
        flags: SCNetworkReachabilityFlags,
        info: *mut c_void,
    ) {
        // SAFETY: `info` is the `State` `start` registered, alive while the callback is.
        let state = unsafe { &*info.cast::<State>() };
        let flipped = state
            .stack
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .update_v6(online(flags));
        if flipped {
            (state.changed)();
        }
    }

    impl Drop for Watch {
        fn drop(&mut self) {
            // SAFETY: unregistering takes no pointers.
            unsafe {
                self.v4.set_dispatch_queue(None);
                self.v4.set_callback(None, std::ptr::null_mut());
                self.v6.set_dispatch_queue(None);
                self.v6.set_callback(None, std::ptr::null_mut());
            }
            // Any callback already on the queue runs before this does.
            self.queue.exec_sync(|| {});
            // SAFETY: `state` came from `Box::leak`, and nothing can reach it any more.
            drop(unsafe { Box::from_raw(self.state.as_ptr()) });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DualStack;

    #[test]
    fn ipv6_only_and_dual_stack_transitions_report_online_when_either_route_is_up() {
        let mut stack = DualStack::new(false, true);
        assert!(stack.is_online(), "IPv6-only / NAT64 starts online");
        assert!(!stack.update_v4(false));
        assert!(
            stack.update_v6(false),
            "losing the last route flips offline"
        );
        assert!(!stack.is_online());
        assert!(stack.update_v4(true), "gaining IPv4 flips back online");
        assert!(
            !stack.update_v6(true),
            "adding IPv6 while online does not re-fire"
        );
        assert!(
            !stack.update_v4(false),
            "dropping IPv4 while IPv6 stays up keeps online"
        );
        assert!(stack.is_online());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn reachable_flags_require_no_connection_needed() {
        use objc2_system_configuration::SCNetworkReachabilityFlags as Flags;
        assert!(super::macos::online(Flags::Reachable));
        assert!(!super::macos::online(Flags(0)));
        assert!(!super::macos::online(
            Flags::Reachable | Flags::ConnectionRequired
        ));
    }
}
