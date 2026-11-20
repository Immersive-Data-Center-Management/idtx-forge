//! Bounded bridge to the OS resolver. A libc lookup cannot be interrupted, so
//! use a fixed worker/queue ceiling and let the caller abandon it at its deadline.

use std::io;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

const WORKERS: usize = 4;
const QUEUE_CAPACITY: usize = 16;

struct Job {
    host: String,
    port: u16,
    deadline: Instant,
    reply: mpsc::SyncSender<io::Result<Vec<SocketAddr>>>,
}

struct Resolver {
    jobs: mpsc::SyncSender<Job>,
}

impl Resolver {
    fn new(
        workers: usize,
        capacity: usize,
        lookup: impl Fn(&str, u16) -> io::Result<Vec<SocketAddr>> + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let (jobs, rx) = mpsc::sync_channel::<Job>(capacity);
        let rx = Arc::new(Mutex::new(rx));
        let lookup = Arc::new(lookup);
        for index in 0..workers {
            let rx = rx.clone();
            let lookup = lookup.clone();
            std::thread::Builder::new().name(format!("idtx-dns-{index}")).spawn(move || {
                loop {
                    let job = rx.lock().unwrap_or_else(std::sync::PoisonError::into_inner).recv();
                    let Ok(job) = job else { break; };
                    if Instant::now() >= job.deadline { continue; }
                    let result = lookup(&job.host, job.port);
                    let _ = job.reply.try_send(result);
                }
            })?;
        }
        Ok(Self { jobs })
    }

    fn resolve(&self, host: &str, port: u16, timeout: Duration) -> io::Result<Vec<SocketAddr>> {
        if timeout.is_zero() { return Err(io::ErrorKind::TimedOut.into()); }
        let deadline = Instant::now() + timeout;
        let (reply, rx) = mpsc::sync_channel(1);
        self.jobs.try_send(Job { host: host.into(), port, deadline, reply })
            .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "DNS worker queue is at capacity"))?;
        rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "DNS resolution timed out"))?
    }
}

pub fn resolve(host: &str, port: u16, timeout: Duration) -> io::Result<Vec<SocketAddr>> {
    static RESOLVER: OnceLock<io::Result<Resolver>> = OnceLock::new();
    RESOLVER.get_or_init(|| Resolver::new(WORKERS, QUEUE_CAPACITY, |host, port| {
        (host, port).to_socket_addrs().map(Iterator::collect)
    })).as_ref().map_err(|_| io::Error::other("DNS workers unavailable"))?
        .resolve(host, port, timeout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stalled_lookup_has_bounded_wait_and_bounded_pending_work() {
        let (release, wait) = mpsc::channel();
        let wait = Mutex::new(wait);
        let (started, started_rx) = mpsc::channel();
        let resolver = Resolver::new(1, 1, move |_, _| {
            let _ = started.send(());
            let _ = wait.lock().unwrap().recv_timeout(Duration::from_secs(2));
            Ok(vec!["1.1.1.1:80".parse().unwrap()])
        }).unwrap();
        let begin = Instant::now();
        assert_eq!(resolver.resolve("one.test", 80, Duration::from_millis(30)).unwrap_err().kind(), io::ErrorKind::TimedOut);
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(resolver.resolve("two.test", 80, Duration::from_millis(30)).unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert_eq!(resolver.resolve("three.test", 80, Duration::from_secs(1)).unwrap_err().kind(), io::ErrorKind::WouldBlock);
        assert!(begin.elapsed() < Duration::from_secs(1));
        release.send(()).unwrap();
    }
}
