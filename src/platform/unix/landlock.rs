use nix::errno::Errno;
use std::ffi::{CStr, CString};

pub(super) struct LandlockConfig {
    pub write_requested: bool,
    pub warn_only: bool,
    pub no_dev: bool,
    pub preset_names: Vec<&'static str>,
    pub writable_dirs: Vec<CString>,
    pub bind_tcp_ports: Vec<u16>,
    pub connect_tcp_ports: Vec<u16>,
}

#[derive(Debug)]
pub(super) enum LandlockError<'a> {
    NotSupported,
    AbiTooOld {
        feature: &'static str,
        required_abi: u32,
        current_abi: u32,
    },
    QueryAbi(Errno),
    CreateRuleset(Errno),
    OpenDir {
        path: &'a CStr,
        errno: Errno,
    },
    AddRule {
        path: &'a CStr,
        errno: Errno,
    },
    AddNetPortRule {
        port: u16,
        action: &'static str,
        errno: Errno,
    },
    SetNoNewPrivs(Errno),
    RestrictSelf(Errno),
}

const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1;
const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;
const LANDLOCK_RULE_NET_PORT: u32 = 2;

const LANDLOCK_ACCESS_FS_WRITE_FILE: u64 = 2;
const LANDLOCK_ACCESS_FS_REMOVE_DIR: u64 = 16;
const LANDLOCK_ACCESS_FS_REMOVE_FILE: u64 = 32;
const LANDLOCK_ACCESS_FS_MAKE_CHAR: u64 = 64;
const LANDLOCK_ACCESS_FS_MAKE_DIR: u64 = 128;
const LANDLOCK_ACCESS_FS_MAKE_REG: u64 = 256;
const LANDLOCK_ACCESS_FS_MAKE_SOCK: u64 = 512;
const LANDLOCK_ACCESS_FS_MAKE_FIFO: u64 = 1024;
const LANDLOCK_ACCESS_FS_MAKE_BLOCK: u64 = 2048;
const LANDLOCK_ACCESS_FS_MAKE_SYM: u64 = 4096;
const LANDLOCK_ACCESS_FS_REFER: u64 = 8192;
const LANDLOCK_ACCESS_FS_TRUNCATE: u64 = 16384;
const LANDLOCK_ACCESS_NET_BIND_TCP: u64 = 1;
const LANDLOCK_ACCESS_NET_CONNECT_TCP: u64 = 2;

#[repr(C)]
struct LandlockRulesetAttrV1 {
    handled_access_fs: u64,
}

#[repr(C)]
struct LandlockRulesetAttrV4 {
    handled_access_fs: u64,
    handled_access_net: u64,
}

#[repr(C)]
struct LandlockPathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

#[repr(C)]
struct LandlockNetPortAttr {
    allowed_access: u64,
    port: u64,
}

struct OwnedFd(i32);

impl Drop for OwnedFd {
    fn drop(&mut self) {
        // SAFETY: the file descriptor is owned by this wrapper and must be closed exactly once.
        unsafe {
            libc::close(self.0);
        }
    }
}

pub(super) fn apply(config: &LandlockConfig) -> Result<u32, LandlockError<'_>> {
    let abi_version = match query_abi_version() {
        Ok(Some(version)) => version,
        Ok(None) => return Err(LandlockError::NotSupported),
        Err(errno) => return Err(LandlockError::QueryAbi(errno)),
    };

    let handled_access_fs = if config.write_requested {
        handled_write_access_fs(abi_version)
    } else {
        0
    };
    let handled_access_net = handled_network_access(
        abi_version,
        !config.bind_tcp_ports.is_empty(),
        !config.connect_tcp_ports.is_empty(),
    )?;
    if handled_access_fs == 0 && handled_access_net == 0 {
        return Err(LandlockError::NotSupported);
    }
    let allowed_writes = allowed_write_access_fs(abi_version);

    let ruleset_fd = match create_ruleset(abi_version, handled_access_fs, handled_access_net) {
        Ok(fd) => OwnedFd(fd),
        Err(errno) if errno_indicates_not_supported(errno) => {
            return Err(LandlockError::NotSupported);
        }
        Err(errno) => return Err(LandlockError::CreateRuleset(errno)),
    };

    if handled_access_fs != 0 && !config.no_dev {
        let dev_dir = c"/dev";
        if let Err(err) =
            add_writable_dir_rule(ruleset_fd.0, dev_dir, LANDLOCK_ACCESS_FS_WRITE_FILE)
        {
            match err {
                LandlockError::OpenDir {
                    errno: Errno::ENOENT,
                    ..
                } => {}
                other => return Err(other),
            }
        }
    }

    for dir in &config.writable_dirs {
        add_writable_dir_rule(ruleset_fd.0, dir.as_c_str(), allowed_writes)?;
    }

    for port in &config.bind_tcp_ports {
        add_net_port_rule(
            ruleset_fd.0,
            *port,
            LANDLOCK_ACCESS_NET_BIND_TCP,
            "bind TCP",
        )?;
    }

    for port in &config.connect_tcp_ports {
        add_net_port_rule(
            ruleset_fd.0,
            *port,
            LANDLOCK_ACCESS_NET_CONNECT_TCP,
            "connect TCP",
        )?;
    }

    set_no_new_privs().map_err(LandlockError::SetNoNewPrivs)?;
    match restrict_self(ruleset_fd.0) {
        Ok(()) => {}
        Err(errno) if errno_indicates_not_supported(errno) => {
            return Err(LandlockError::NotSupported);
        }
        Err(errno) => return Err(LandlockError::RestrictSelf(errno)),
    }

    Ok(abi_version)
}

fn handled_write_access_fs(abi_version: u32) -> u64 {
    let mut handled = LANDLOCK_ACCESS_FS_WRITE_FILE
        | LANDLOCK_ACCESS_FS_REMOVE_DIR
        | LANDLOCK_ACCESS_FS_REMOVE_FILE
        | LANDLOCK_ACCESS_FS_MAKE_CHAR
        | LANDLOCK_ACCESS_FS_MAKE_DIR
        | LANDLOCK_ACCESS_FS_MAKE_REG
        | LANDLOCK_ACCESS_FS_MAKE_SOCK
        | LANDLOCK_ACCESS_FS_MAKE_FIFO
        | LANDLOCK_ACCESS_FS_MAKE_BLOCK
        | LANDLOCK_ACCESS_FS_MAKE_SYM;
    if abi_version >= 2 {
        handled |= LANDLOCK_ACCESS_FS_REFER;
    }
    if abi_version >= 3 {
        handled |= LANDLOCK_ACCESS_FS_TRUNCATE;
    }
    handled
}

fn allowed_write_access_fs(abi_version: u32) -> u64 {
    handled_write_access_fs(abi_version)
        & !(LANDLOCK_ACCESS_FS_MAKE_CHAR | LANDLOCK_ACCESS_FS_MAKE_BLOCK)
}

fn handled_network_access(
    abi_version: u32,
    allow_bind_tcp: bool,
    allow_connect_tcp: bool,
) -> Result<u64, LandlockError<'static>> {
    if !allow_bind_tcp && !allow_connect_tcp {
        return Ok(0);
    }
    if abi_version < 4 {
        return Err(LandlockError::AbiTooOld {
            feature: "TCP port restrictions",
            required_abi: 4,
            current_abi: abi_version,
        });
    }

    let mut handled = 0;
    if allow_bind_tcp {
        handled |= LANDLOCK_ACCESS_NET_BIND_TCP;
    }
    if allow_connect_tcp {
        handled |= LANDLOCK_ACCESS_NET_CONNECT_TCP;
    }
    Ok(handled)
}

fn query_abi_version() -> std::result::Result<Option<u32>, Errno> {
    // SAFETY: calling a raw syscall with documented parameters and a null pointer for the
    // version query is safe.
    let ret = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if ret == -1 {
        let errno = Errno::last();
        if errno_indicates_not_supported(errno) {
            Ok(None)
        } else {
            Err(errno)
        }
    } else {
        Ok(Some(ret as u32))
    }
}

fn create_ruleset(
    abi_version: u32,
    handled_access_fs: u64,
    handled_access_net: u64,
) -> std::result::Result<i32, Errno> {
    let ret = if abi_version >= 4 {
        let attr = LandlockRulesetAttrV4 {
            handled_access_fs,
            handled_access_net,
        };
        // SAFETY: calling the Landlock create_ruleset syscall with a pointer to a C-compatible
        // struct matching the supported ABI.
        unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                &attr as *const LandlockRulesetAttrV4,
                std::mem::size_of::<LandlockRulesetAttrV4>(),
                0u32,
            )
        }
    } else {
        let attr = LandlockRulesetAttrV1 { handled_access_fs };
        // SAFETY: calling the Landlock create_ruleset syscall with a pointer to a C-compatible
        // struct matching the supported ABI.
        unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                &attr as *const LandlockRulesetAttrV1,
                std::mem::size_of::<LandlockRulesetAttrV1>(),
                0u32,
            )
        }
    };
    if ret == -1 {
        Err(Errno::last())
    } else {
        Ok(ret as i32)
    }
}

fn add_writable_dir_rule(
    ruleset_fd: i32,
    path: &CStr,
    allowed_access: u64,
) -> Result<(), LandlockError<'_>> {
    let dir_fd = open_path_dir(path).map_err(|errno| LandlockError::OpenDir { path, errno })?;
    let dir_fd = OwnedFd(dir_fd);
    let attr = LandlockPathBeneathAttr {
        allowed_access,
        parent_fd: dir_fd.0,
    };
    // SAFETY: calling the Landlock add_rule syscall with a pointer to a C-compatible struct.
    let ret = unsafe {
        libc::syscall(
            libc::SYS_landlock_add_rule,
            ruleset_fd,
            LANDLOCK_RULE_PATH_BENEATH,
            &attr as *const LandlockPathBeneathAttr,
            0u32,
        )
    };
    if ret == -1 {
        return Err(LandlockError::AddRule {
            path,
            errno: Errno::last(),
        });
    }
    Ok(())
}

fn add_net_port_rule(
    ruleset_fd: i32,
    port: u16,
    allowed_access: u64,
    action: &'static str,
) -> Result<(), LandlockError<'static>> {
    let attr = LandlockNetPortAttr {
        allowed_access,
        port: u64::from(port),
    };
    // SAFETY: calling the Landlock add_rule syscall with a pointer to a C-compatible struct.
    let ret = unsafe {
        libc::syscall(
            libc::SYS_landlock_add_rule,
            ruleset_fd,
            LANDLOCK_RULE_NET_PORT,
            &attr as *const LandlockNetPortAttr,
            0u32,
        )
    };
    if ret == -1 {
        return Err(LandlockError::AddNetPortRule {
            port,
            action,
            errno: Errno::last(),
        });
    }
    Ok(())
}

fn open_path_dir(path: &CStr) -> std::result::Result<i32, Errno> {
    let flags = libc::O_PATH | libc::O_CLOEXEC | libc::O_DIRECTORY;
    // SAFETY: `path` is NUL-terminated and the libc `open` flags are passed as documented.
    let fd = unsafe { libc::open(path.as_ptr(), flags) };
    if fd == -1 { Err(Errno::last()) } else { Ok(fd) }
}

fn set_no_new_privs() -> std::result::Result<(), Errno> {
    // SAFETY: `prctl` is invoked with documented constants to enable `no_new_privs`.
    let ret = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if ret == -1 {
        Err(Errno::last())
    } else {
        Ok(())
    }
}

fn restrict_self(ruleset_fd: i32) -> std::result::Result<(), Errno> {
    // SAFETY: calling the Landlock restrict_self syscall with a valid ruleset file descriptor.
    let ret = unsafe { libc::syscall(libc::SYS_landlock_restrict_self, ruleset_fd, 0u32) };
    if ret == -1 {
        Err(Errno::last())
    } else {
        Ok(())
    }
}

fn errno_indicates_not_supported(errno: Errno) -> bool {
    errno == Errno::ENOSYS || errno == Errno::EOPNOTSUPP
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowed_write_mask_excludes_device_nodes() {
        let allowed = allowed_write_access_fs(1);
        assert_eq!(allowed & LANDLOCK_ACCESS_FS_MAKE_CHAR, 0);
        assert_eq!(allowed & LANDLOCK_ACCESS_FS_MAKE_BLOCK, 0);
        assert_ne!(allowed & LANDLOCK_ACCESS_FS_WRITE_FILE, 0);
    }

    #[test]
    fn handled_mask_includes_refer_and_truncate_by_version() {
        let v1 = handled_write_access_fs(1);
        assert_eq!(v1 & LANDLOCK_ACCESS_FS_REFER, 0);
        assert_eq!(v1 & LANDLOCK_ACCESS_FS_TRUNCATE, 0);

        let v2 = handled_write_access_fs(2);
        assert_ne!(v2 & LANDLOCK_ACCESS_FS_REFER, 0);
        assert_eq!(v2 & LANDLOCK_ACCESS_FS_TRUNCATE, 0);

        let v3 = handled_write_access_fs(3);
        assert_ne!(v3 & LANDLOCK_ACCESS_FS_REFER, 0);
        assert_ne!(v3 & LANDLOCK_ACCESS_FS_TRUNCATE, 0);
    }

    #[test]
    fn handled_network_access_requires_abi_v4() {
        let err = handled_network_access(3, true, false).unwrap_err();
        assert!(matches!(
            err,
            LandlockError::AbiTooOld {
                feature: "TCP port restrictions",
                required_abi: 4,
                current_abi: 3,
            }
        ));
    }

    #[test]
    fn handled_network_access_tracks_requested_port_actions() {
        assert_eq!(handled_network_access(4, false, false).unwrap(), 0);
        assert_eq!(
            handled_network_access(4, true, false).unwrap(),
            LANDLOCK_ACCESS_NET_BIND_TCP
        );
        assert_eq!(
            handled_network_access(4, false, true).unwrap(),
            LANDLOCK_ACCESS_NET_CONNECT_TCP
        );
        assert_eq!(
            handled_network_access(4, true, true).unwrap(),
            LANDLOCK_ACCESS_NET_BIND_TCP | LANDLOCK_ACCESS_NET_CONNECT_TCP
        );
    }

    #[test]
    fn errno_not_supported_classifies_known_values() {
        assert!(errno_indicates_not_supported(Errno::ENOSYS));
        assert!(errno_indicates_not_supported(Errno::EOPNOTSUPP));
        assert!(!errno_indicates_not_supported(Errno::EPERM));
    }
}
