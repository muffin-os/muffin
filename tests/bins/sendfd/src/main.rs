//! Prints `sendfd: ok`; the first failure prints `sendfd: FAIL <step>` and
//! exits 1.

use std::ffi::{c_char, c_int, c_uint};
use std::fs::File;
use std::io;
use std::mem::{size_of, zeroed};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::{Command, exit};

use muffin_sys::{EACCES, MapFlags, O_CREAT, O_EXCL, O_RDWR, ProtFlags, mmap};

unsafe extern "C" {
    fn shm_open(name: *const c_char, oflag: c_int, mode: c_uint) -> c_int;
}

const SOCKET_PATH: &str = "/sendfd.sock";
const SHM_PATH: &str = "/dev/shm/sendfd";
const SHM_BYTES: usize = 4096;
const PATTERN_BYTES: usize = 256;
const MAX_FDS_PER_MESSAGE: usize = 2;
const CONTROL_CAPACITY: usize =
    libc::CMSG_SPACE((MAX_FDS_PER_MESSAGE * size_of::<c_int>()) as c_uint) as usize;

#[repr(C, align(8))]
struct Control([u8; CONTROL_CAPACITY]);

fn send_fds(sock: c_int, fds: &[c_int]) -> Result<(), &'static str> {
    let mut byte = [1u8];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let mut control = Control([0; CONTROL_CAPACITY]);
    let data_len = size_of_val(fds) as c_uint;
    let space = libc::CMSG_SPACE(data_len) as usize;
    if space > CONTROL_CAPACITY {
        return Err("control capacity");
    }
    let mut msg: libc::msghdr = unsafe { zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.0.as_mut_ptr().cast();
    msg.msg_controllen = space;
    unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        if cmsg.is_null() {
            return Err("CMSG_FIRSTHDR");
        }
        (*cmsg).cmsg_len = libc::CMSG_LEN(data_len) as usize;
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        let data = libc::CMSG_DATA(cmsg).cast::<c_int>();
        for (i, &fd) in fds.iter().enumerate() {
            data.add(i).write_unaligned(fd);
        }
    }
    if unsafe { libc::sendmsg(sock, &msg, 0) } != 1 {
        return Err("sendmsg");
    }
    Ok(())
}

struct Received {
    msg_flags: c_int,
    controllen: usize,
    fds: Vec<c_int>,
}

fn recv_fds(sock: c_int, controllen: usize) -> Result<Received, &'static str> {
    let mut byte = [0u8];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let mut control = Control([0; CONTROL_CAPACITY]);
    let mut msg: libc::msghdr = unsafe { zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.0.as_mut_ptr().cast();
    msg.msg_controllen = controllen;
    if unsafe { libc::recvmsg(sock, &mut msg, 0) } != 1 {
        return Err("recvmsg returns 1");
    }
    let mut fds = Vec::new();
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level != libc::SOL_SOCKET || (*cmsg).cmsg_type != libc::SCM_RIGHTS {
                return Err("cmsg level and type");
            }
            let header = libc::CMSG_LEN(0) as usize;
            let count = ((*cmsg).cmsg_len as usize - header) / size_of::<c_int>();
            let data = libc::CMSG_DATA(cmsg).cast::<c_int>();
            fds.extend((0..count).map(|i| data.add(i).read_unaligned()));
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
    }
    Ok(Received {
        msg_flags: msg.msg_flags,
        controllen: msg.msg_controllen,
        fds,
    })
}

fn client() -> Result<(), &'static str> {
    let stream = UnixStream::connect(SOCKET_PATH).map_err(|_| "connect")?;
    let fd = unsafe { shm_open(c"/sendfd".as_ptr(), O_RDWR | O_CREAT | O_EXCL, 0o600) };
    if fd < 0 {
        return Err("shm_open");
    }
    let file = unsafe { File::from_raw_fd(fd) };
    file.set_len(SHM_BYTES as u64).map_err(|_| "ftruncate")?;
    let addr = mmap(
        0,
        SHM_BYTES,
        ProtFlags::READ | ProtFlags::WRITE,
        MapFlags::SHARED,
        fd,
        0,
    )
    .map_err(|_| "mmap")?;
    for i in 0..PATTERN_BYTES {
        unsafe { addr.add(i).write_volatile(i as u8) };
    }
    send_fds(stream.as_raw_fd(), &[fd])?;
    send_fds(stream.as_raw_fd(), &[0, fd]).map_err(|_| "second sendmsg")?;
    Ok(())
}

fn server() -> Result<(), &'static str> {
    let listener = UnixListener::bind(SOCKET_PATH).map_err(|_| "bind")?;
    let exe = std::env::current_exe().map_err(|_| "current_exe")?;
    let mut child = Command::new(exe)
        .arg("client")
        .spawn()
        .map_err(|_| "spawn")?;
    let (stream, _) = listener.accept().map_err(|_| "accept")?;
    let sock = stream.as_raw_fd();

    let space = libc::CMSG_SPACE(size_of::<c_int>() as c_uint) as usize;
    let first = recv_fds(sock, space)?;
    if first.controllen != space || first.msg_flags & libc::MSG_CTRUNC != 0 || first.fds.len() != 1
    {
        return Err("recvmsg");
    }

    let addr = mmap(
        0,
        SHM_BYTES,
        ProtFlags::READ,
        MapFlags::SHARED,
        first.fds[0],
        0,
    )
    .map_err(|_| "pattern mmap")?;
    if !(0..PATTERN_BYTES).all(|i| unsafe { addr.add(i).read_volatile() } == i as u8) {
        return Err("pattern");
    }

    let owner = File::open(SHM_PATH);
    if !owner
        .as_ref()
        .err()
        .and_then(io::Error::raw_os_error)
        .is_some_and(|code| code == c_int::from(EACCES))
    {
        return Err("owner");
    }

    let len = libc::CMSG_LEN(size_of::<c_int>() as c_uint) as usize;
    let second = recv_fds(sock, len).map_err(|_| "truncate")?;
    if second.msg_flags & libc::MSG_CTRUNC == 0 || second.controllen != len || second.fds.len() != 1
    {
        return Err("truncate");
    }

    if !child.wait().is_ok_and(|status| status.success()) {
        return Err("wait");
    }
    Ok(())
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("client") {
        if let Err(step) = client() {
            println!("sendfd: FAIL client {step}");
            exit(1);
        }
        exit(0);
    }
    if let Err(step) = server() {
        println!("sendfd: FAIL {step}");
        exit(1);
    }
    println!("sendfd: ok");
}
