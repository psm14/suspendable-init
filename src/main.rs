use nix::sys::signal::{self, Signal};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::Pid;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::{thread, time};

static RUNNING: AtomicBool = AtomicBool::new(true);
static CHILD_PID: Mutex<Option<Pid>> = Mutex::new(None);


extern "C" fn handle_signal(sig: i32) {
    match Signal::try_from(sig) {
        Ok(Signal::SIGCHLD) => {
            // SIGCHLD is handled in the main loop
        },
        Ok(Signal::SIGKILL) => {
            println!("Received termination signal");
            RUNNING.store(false, Ordering::SeqCst);
        },
        Ok(signal) => {
            // Forward other signals to the child process
            if let Some(pid) = *CHILD_PID.lock().unwrap() {
                let _ = signal::kill(pid, signal);
            }
        },
        Err(_) => {
            // Ignore unknown signals
        }
    }
}

fn setup_signal_handlers() {
    unsafe {
        signal::signal(Signal::SIGCHLD, signal::SigHandler::Handler(handle_signal)).unwrap();
        signal::signal(Signal::SIGTERM, signal::SigHandler::Handler(handle_signal)).unwrap();
        signal::signal(Signal::SIGINT, signal::SigHandler::Handler(handle_signal)).unwrap();
    }
}

fn reap_children() {
    loop {
        match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => break,
            Ok(status) => println!("Reaped process with status: {:?}", status),
            Err(nix::errno::Errno::ECHILD) => break, // No more children
            Err(e) => {
                eprintln!("Error waiting for child process: {:?}", e);
                break;
            }
        }
    }
}

fn wait_for_child(child: &mut Child) -> Option<std::process::ExitStatus> {
    match child.try_wait() {
        Ok(Some(status)) => Some(status),
        Ok(None) => None,
        Err(e) => {
            eprintln!("Error checking child process: {:?}", e);
            None
        }
    }
}

fn spawn_child_process() -> Result<Child, std::io::Error> {
    let command = std::env::args().nth(1).expect("No command provided");
    let args: Vec<String> = std::env::args().skip(2).collect();
    let child = Command::new(command)
        .args(args)
        .spawn()
        .expect("Failed to start application");
    Ok(child)
}

fn main() {
    setup_signal_handlers();

    println!("PID 1 init process started");

    let mut child = match spawn_child_process() {
        Ok(child) => child,
        Err(e) => {
            eprintln!("Failed to start child process: {}", e);
            std::process::exit(1);
        }
    };

    // Store the child's PID
    *CHILD_PID.lock().unwrap() = Some(Pid::from_raw(child.id() as i32));

    while RUNNING.load(Ordering::SeqCst) {
        if let Some(status) = wait_for_child(&mut child) {
            println!("Child process exited with status: {:?}", status);
            break;
        }

        // Reap any other child processes that might have terminated
        reap_children();

        thread::sleep(time::Duration::from_millis(100));
    }

    // If we're here because of a signal, try to terminate the child gracefully
    if !RUNNING.load(Ordering::SeqCst) {
        println!("Terminating child process");
        let _ = child.kill();
        let _ = child.wait(); // Ensure we don't leave a zombie
    }

    // Final reap to ensure all children are properly waited for
    reap_children();

    println!("PID 1 init process exiting");
}