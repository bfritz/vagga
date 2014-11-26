use std::io::IoError;
use std::os::errno;
use std::ptr::null;
use std::time::Duration;
use std::collections::PriorityQueue;
use std::collections::HashMap;
use time::{Timespec, get_time};
use libc::{c_int, c_void};
use libc::consts::os::posix88::{EINTR, ETIMEDOUT, EAGAIN, ECHILD};
use libc::consts::os::posix88::{SIGTERM, SIGINT, SIGQUIT};

use super::signal;

static SIGCHLD: c_int = 17;

pub enum Event<Name> {
    Signal(signal::Signal),
    Timeout(Name),
    Input(Name),
}

struct FileDesc(c_int);

struct Unordered<T>(T);

impl<T> PartialOrd for Unordered<T> {
    fn partial_cmp(&self, other: &Unordered<T>) -> Option<Ordering> { Some(Equal) }
}
impl<T> Ord for Unordered<T> {
    fn cmp(&self, other: &Unordered<T>) -> Ordering { Equal }
}
impl<T> PartialEq for Unordered<T> {
    fn eq(&self, other: &Unordered<T>) -> bool { false }
}
impl<T> Eq for Unordered<T> {}

pub struct Loop<Name> {
    queue: PriorityQueue<(i64, Unordered<Name>)>,
    epoll_fd: FileDesc,
    signal_fd: FileDesc,
    inputs: HashMap<c_int, Name>,
}

extern {
    fn create_epoll() -> c_int;
    fn create_signalfd() -> c_int;
    fn close(fd: c_int) -> c_int;
    fn epoll_add_read(efd: c_int, fd: c_int) -> c_int;
    fn epoll_wait_read(epfd: c_int, timeout: c_int) -> c_int;
    fn read_signal(rd: c_int) -> c_int;
}

pub fn time_to_ms(ts: Timespec) -> i64 {
    return (ts.sec as i64) * 1000 + (ts.nsec as i64) / 1000000;
}

pub fn get_time_ms() -> i64 {
    return time_to_ms(get_time());
}

impl<Name: Clone> Loop<Name> {
    pub fn new() -> Result<Loop<Name>, IoError> {
        let efd = unsafe { create_epoll() };
        if efd < 0 {
            return Err(IoError::last_error());
        }
        let epoll = FileDesc(efd);
        let sfd = unsafe { create_signalfd() };
        if sfd < 0 {
            return Err(IoError::last_error());
        }
        let sig = FileDesc(sfd);
        if unsafe { epoll_add_read(efd, sfd) } < 0 {
            return Err(IoError::last_error());
        }
        return Ok(Loop {
            queue: PriorityQueue::new(),
            epoll_fd: epoll,
            signal_fd: sig,
            inputs: HashMap::new(),
        });
    }
    pub fn add_timeout(&mut self, duration: Duration, name: Name) {
        self.queue.push((-time_to_ms(get_time() + duration), Unordered(name)));
    }
    fn get_timeout(&mut self) -> c_int {
        self.queue.top()
            .map(|&(ts, _)| (-ts) - get_time_ms())
            .map(|ts| if ts >= 0 { ts } else { 0 })
            .unwrap_or(-1)
            as i32
    }
    pub fn poll(&mut self) -> Event<Name> {
        loop {
            if let Some(sig) = signal::check_children() {
                return Signal(sig);
            }
            let timeo = self.get_timeout();
            let FileDesc(sfd) = self.signal_fd;
            let FileDesc(efd) = self.epoll_fd;
            let fd = unsafe { epoll_wait_read(efd, timeo) };
            if fd == -ETIMEDOUT { // Timeout
                debug!("Timeout");
                if self.get_timeout() != 0 {
                    continue;
                }
                let (_, Unordered(name)) = self.queue.pop().unwrap();
                return Timeout(name);
            } else if fd == -EINTR {
                continue
            } else if fd < 0 {
                fail!(format!("Error in epoll: {}", IoError::last_error()));
            } else if fd == sfd { // Signal
                debug!("Signal");
                let rc =  unsafe { read_signal(sfd) };
                if rc == EINTR || rc == EAGAIN {
                    continue;
                } else if(rc <= 0) {
                    fail!(format!("Error in read_signal: {}",
                        IoError::last_error()));
                } else {
                    match rc {
                        sig@SIGTERM | sig@SIGINT | sig@SIGQUIT => {
                            return Signal(signal::Terminate(sig as int));
                        }
                        SIGCHLD => {
                            continue;  // Will waitpid on next iteration
                        }
                        _ => {
                            warn!("Signal {} ignored", rc);
                            continue;
                        }
                    }
                }
            } else {
                debug!("Input {}", fd);
                return Input(self.inputs[fd].clone());
            }
            unreachable!();
        }
    }
}

impl Drop for FileDesc {
    fn drop(&mut self) {
        let FileDesc(fd) = *self;
        unsafe { close(fd) };
    }
}
