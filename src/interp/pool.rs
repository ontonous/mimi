// Receiver lock poisoning panic is intentional.
#![cfg_attr(not(test), allow(clippy::unwrap_used))]
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::thread;

pub(crate) struct ThreadPool {
    workers: Vec<thread::JoinHandle<()>>,
    sender: mpsc::Sender<Box<dyn FnOnce() + Send + 'static>>,
}

impl Drop for ThreadPool {
    fn drop(&mut self) {
        // Drop the sender first to signal workers (recv returns Err, loop breaks)
        // Then join all worker threads to ensure clean shutdown.
        for handle in self.workers.drain(..) {
            let _ = handle.join();
        }
    }
}

impl ThreadPool {
    pub(crate) fn new(size: usize) -> Self {
        let (sender, receiver) = mpsc::channel::<Box<dyn FnOnce() + Send + 'static>>();
        let receiver = Arc::new(Mutex::new(receiver));
        let mut workers = Vec::with_capacity(size);

        for _ in 0..size {
            let receiver = Arc::clone(&receiver);
            let worker = thread::spawn(move || loop {
                let task = receiver.lock().unwrap_or_else(|e| e.into_inner()).recv();
                match task {
                    Ok(task) => task(),
                    Err(_) => break,
                }
            });
            workers.push(worker);
        }

        ThreadPool { workers, sender }
    }

    pub(crate) fn execute<F: FnOnce() + Send + 'static>(&self, job: F) {
        if let Err(e) = self.sender.send(Box::new(job)) {
            eprintln!("[pool] failed to send task: {}", e);
        }
    }
}

static POOL: OnceLock<ThreadPool> = OnceLock::new();

pub(crate) fn get_pool() -> &'static ThreadPool {
    POOL.get_or_init(|| {
        let size = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        ThreadPool::new(size)
    })
}
