use nix::sys::signal::{self, Signal};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::Pid;
use std::fs;
use std::path::Path;
use std::process::{Child, Command, ExitCode};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::{thread, time};

static KILLED: AtomicBool = AtomicBool::new(false);
static INTERRUPTED: AtomicBool = AtomicBool::new(false);
static CHILD_PID: Mutex<Option<Pid>> = Mutex::new(None);
static IDE_ATTACHED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_signal(sig: i32) {
    match Signal::try_from(sig) {
        Ok(Signal::SIGCHLD) => {
            // SIGCHLD is handled in the main loop
        },
        Ok(Signal::SIGKILL) => {
            println!("Received termination signal");
            KILLED.store(true, Ordering::SeqCst);
        },
        Ok(Signal::SIGINT) => {
            println!("Received interrupt signal");
            INTERRUPTED.store(true, Ordering::SeqCst);
            if let Some(pid) = *CHILD_PID.lock().unwrap() {
                let _ = signal::kill(pid, Signal::SIGINT);
            }
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

fn is_ide_attached() -> bool {
    let proc_dir = Path::new("/proc");
    
    if let Ok(entries) = fs::read_dir(proc_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(file_name) = path.file_name() {
                if let Ok(pid) = file_name.to_string_lossy().parse::<i32>() {
                    if pid != 1 && !is_descendant_of_init(pid) {
                        return true;
                    }
                }
            }
        }
    }
    
    false
}

fn is_descendant_of_init(pid: i32) -> bool {
    let mut current_pid = pid;
    
    while current_pid != 1 {
        if let Some(ppid) = get_parent_pid(current_pid) {
            current_pid = ppid;
        } else {
            return false;
        }
    }
    
    true
}

fn get_parent_pid(pid: i32) -> Option<i32> {
    let status_file = format!("/proc/{}/status", pid);
    if let Ok(content) = fs::read_to_string(status_file) {
        for line in content.lines() {
            if line.starts_with("PPid:") {
                if let Some(ppid_str) = line.split_whitespace().nth(1) {
                    return ppid_str.parse().ok();
                }
            }
        }
    }
    None
}

fn main() -> ExitCode {
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

    let mut status: u8 = 0;

    while !KILLED.load(Ordering::SeqCst) && !INTERRUPTED.load(Ordering::SeqCst) {
        let ide_attached = IDE_ATTACHED.load(Ordering::SeqCst);
        if is_ide_attached() != ide_attached {
            let new_state = !ide_attached;
            IDE_ATTACHED.store(new_state, Ordering::SeqCst);
            if new_state {
                println!("IDE attached, stopping child process");
                let _ = child.kill();
                let _ = child.wait();
            } else {
                println!("IDE detached, restarting child process");
                child = spawn_child_process().expect("Failed to restart child process");
                *CHILD_PID.lock().unwrap() = Some(Pid::from_raw(child.id() as i32));
            }
        }

        if !IDE_ATTACHED.load(Ordering::SeqCst) {
            if let Some(child_status) = wait_for_child(&mut child) {
                if let Some(unix_code) = child_status.code() {
                    println!("Child process exited with status: {:?}", unix_code);
                    status = unix_code as u8;
                }
                break;
            }
        }

        // Reap any other child processes that might have terminated
        reap_children();

        thread::sleep(time::Duration::from_millis(100));
    }

    // If we're here because of a signal, try to terminate the child gracefully
    if KILLED.load(Ordering::SeqCst) {
        println!("Terminating child process");
        let _ = child.kill();
        if let Ok(child_status) = child.wait() {
            if let Some(unix_code) = child_status.code() {
                status = unix_code as u8;
            }
        }
    } else if INTERRUPTED.load(Ordering::SeqCst) {
        if let Ok(child_status) = child.wait() {
            if let Some(unix_code) = child_status.code() {
                status = unix_code as u8;
            }
        }
    }

    // Final reap to ensure all children are properly waited for
    reap_children();

    println!("PID 1 init process exiting");

    ExitCode::from(status)
}