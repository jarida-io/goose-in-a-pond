//! The host's IPv4 addresses, read live, for the advertising decision and a Wi-Fi join's wait for
//! an address. The judgement of which ones count is pond-core's.

use std::io;
use std::net::Ipv4Addr;

use pond_core::network::domain::interface;

/// Whether the pond has a network a phone could reach it on; a seam so the advertising
/// controller is tested without the host's own interfaces.
pub trait NetworkProbe: Send + Sync {
    fn has_usable_network(&self) -> bool;
}

/// This host's interfaces.
#[derive(Debug, Default, Clone, Copy)]
pub struct HostNetwork;

impl NetworkProbe for HostNetwork {
    fn has_usable_network(&self) -> bool {
        match ipv4_addresses() {
            Ok(addresses) => interface::has_usable_network(
                addresses.iter().map(|(name, ip)| (name.as_str(), *ip)),
            ),
            Err(error) => {
                // Read as offline: that only ever offers more setup, never less.
                tracing::warn!(%error, "could not read this host's addresses; treating it as offline");
                false
            }
        }
    }
}

/// Every IPv4 address on an interface that is up and not point-to-point, with its interface name.
#[cfg(unix)]
pub fn ipv4_addresses() -> io::Result<Vec<(String, Ipv4Addr)>> {
    struct List(*mut libc::ifaddrs);
    impl Drop for List {
        fn drop(&mut self) {
            // SAFETY: the pointer came from a successful getifaddrs and is freed exactly once.
            unsafe { libc::freeifaddrs(self.0) };
        }
    }

    let mut head = std::mem::MaybeUninit::<*mut libc::ifaddrs>::uninit();
    // SAFETY: getifaddrs writes the head of a list libc owns when it returns 0.
    if unsafe { libc::getifaddrs(head.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: written by the successful call above; null means no interfaces at all.
    let list = List(unsafe { head.assume_init() });
    // SAFETY: every entry stays allocated until `list` drops, after the walk returns.
    Ok(unsafe { ipv4_entries(std::ptr::NonNull::new(list.0)) })
}

/// The walk itself, apart from getifaddrs, so its edge cases are tested on a list built by hand.
///
/// # Safety
/// Every entry reachable from `head` through `ifa_next` must stay valid until this returns.
#[cfg(unix)]
unsafe fn ipv4_entries(head: Option<std::ptr::NonNull<libc::ifaddrs>>) -> Vec<(String, Ipv4Addr)> {
    let mut addresses = Vec::new();
    let mut current = head;
    while let Some(entry) = current {
        // SAFETY: non-null, and valid for the whole walk by this function's contract.
        let entry = unsafe { entry.as_ref() };
        current = std::ptr::NonNull::new(entry.ifa_next);
        let flags = entry.ifa_flags as libc::c_int;
        if flags & libc::IFF_UP == 0 || flags & libc::IFF_POINTOPOINT != 0 {
            continue;
        }
        let (Some(addr), Some(name)) = (
            std::ptr::NonNull::new(entry.ifa_addr),
            std::ptr::NonNull::new(entry.ifa_name),
        ) else {
            continue;
        };
        // SAFETY: a non-null ifa_addr points at a sockaddr of at least sa_family's size.
        let family = libc::c_int::from(unsafe { addr.as_ref() }.sa_family);
        if family != libc::AF_INET {
            continue;
        }
        // SAFETY: an AF_INET entry's address is a sockaddr_in; read unaligned to assume nothing.
        let address =
            unsafe { std::ptr::read_unaligned(addr.as_ptr().cast::<libc::sockaddr_in>()) };
        // SAFETY: getifaddrs guarantees a nul-terminated interface name.
        let name = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        addresses.push((name, Ipv4Addr::from(u32::from_be(address.sin_addr.s_addr))));
    }
    addresses
}

#[cfg(not(unix))]
pub fn ipv4_addresses() -> io::Result<Vec<(String, Ipv4Addr)>> {
    Ok(Vec::new())
}

/// The first IPv4 address `interface_name` holds now that is not link-local.
pub fn ipv4_on(interface_name: &str) -> Option<Ipv4Addr> {
    ipv4_addresses()
        .ok()?
        .into_iter()
        .find(|(name, ip)| name == interface_name && !ip.is_link_local() && !ip.is_loopback())
        .map(|(_, ip)| ip)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn sockaddr_in(ip: Ipv4Addr) -> libc::sockaddr_in {
        // SAFETY: all-zero is a valid sockaddr_in on every Unix; only the fields set below matter.
        let mut address: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        address.sin_family = libc::AF_INET as libc::sa_family_t;
        address.sin_addr.s_addr = u32::from(ip).to_be();
        address
    }

    fn entry(
        name: &std::ffi::CStr,
        flags: libc::c_int,
        addr: *mut libc::sockaddr,
    ) -> libc::ifaddrs {
        // SAFETY: all-zero is a valid ifaddrs: every pointer null, every flag clear.
        let mut entry: libc::ifaddrs = unsafe { std::mem::zeroed() };
        entry.ifa_name = name.as_ptr().cast_mut();
        entry.ifa_flags = flags as _;
        entry.ifa_addr = addr;
        entry
    }

    /// Every entry the walk must pass over, each on its own, ahead of the one it must keep.
    #[test]
    fn the_walk_keeps_only_ipv4_on_interfaces_that_are_up_and_not_point_to_point() {
        let mut kept = sockaddr_in(Ipv4Addr::new(192, 168, 1, 42));
        let mut down = sockaddr_in(Ipv4Addr::new(10, 0, 0, 1));
        let mut tunnel = sockaddr_in(Ipv4Addr::new(100, 64, 0, 9));
        let mut unnamed = sockaddr_in(Ipv4Addr::new(10, 0, 0, 2));
        // SAFETY: all-zero is a valid sockaddr_in6.
        let mut v6: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
        v6.sin6_family = libc::AF_INET6 as libc::sa_family_t;
        let up = libc::IFF_UP;
        let as_sockaddr = |address: &mut libc::sockaddr_in| -> *mut libc::sockaddr {
            (address as *mut libc::sockaddr_in).cast()
        };

        let mut list = [
            entry(c"en1", 0, as_sockaddr(&mut down)),
            entry(
                c"utun0",
                up | libc::IFF_POINTOPOINT,
                as_sockaddr(&mut tunnel),
            ),
            entry(c"en2", up, std::ptr::null_mut()),
            entry(c"en3", up, (&mut v6 as *mut libc::sockaddr_in6).cast()),
            entry(c"en4", up, as_sockaddr(&mut unnamed)),
            entry(c"en0", up, as_sockaddr(&mut kept)),
        ];
        list[4].ifa_name = std::ptr::null_mut();
        // Every link derives from one raw pointer, so no later borrow invalidates an earlier link.
        let base = list.as_mut_ptr();
        for i in 0..list.len() - 1 {
            // SAFETY: both indices are inside `list`.
            unsafe { (*base.add(i)).ifa_next = base.add(i + 1) };
        }

        // SAFETY: every entry lives on this stack frame until the walk returns.
        let addresses = unsafe { ipv4_entries(std::ptr::NonNull::new(base)) };
        assert_eq!(
            addresses,
            vec![("en0".to_string(), Ipv4Addr::new(192, 168, 1, 42))]
        );
        // SAFETY: an empty list is the walk's other edge.
        assert!(unsafe { ipv4_entries(None) }.is_empty());
    }

    #[test]
    fn the_walk_reaches_loopback() {
        let addresses = ipv4_addresses().expect("getifaddrs");
        assert!(
            addresses.iter().any(|(_, ip)| ip.is_loopback()),
            "no loopback among {addresses:?}"
        );
        assert_eq!(
            ipv4_on("lo0").or(ipv4_on("lo")),
            None,
            "loopback is never a network"
        );
    }
}
