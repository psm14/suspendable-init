use nix::sys::signal::{self, Signal, SigSet};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::Pid;
use std::process::{Child, Command, ExitCode, ExitStatus};

fn reap_zombies() {
    loop {
        match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => break,
            Ok(_) => { /* Success */ },
            Err(nix::errno::Errno::ECHILD) => break, // No more children
            Err(e) => {
                eprintln!("Error waiting for child process: {:?}", e);
                break;
            }
        }
    }
}

extern "C" fn handle_signal(_sig: i32) {
    reap_zombies();
}

fn setup_signal_handlers() {
    unsafe {
        signal::signal(Signal::SIGCHLD, signal::SigHandler::Handler(handle_signal)).unwrap();
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

fn exit_status_to_exit_code(status: ExitStatus) -> ExitCode {
    if let Some(code) = status.code() {
        ExitCode::from(code as u8)
    } else {
        // This can happen if the process was terminated by a signal
        // Here we choose a generic exit code, like 1, to indicate an error
        ExitCode::from(1)
    }
}

fn main() -> ExitCode {
    setup_signal_handlers();

    let mut running = true;
    let mut proc = match spawn_child_process() {
        Ok(child) => child,
        Err(e) => {
            eprintln!("Failed to start child process: {}", e);
            return ExitCode::FAILURE;
        }
    };

    let sigset = SigSet::all();
    sigset.thread_block().expect("Failed to block signals");

    while let Ok(signal) = sigset.wait() {
        println!("{:?}", signal);
        match signal {
            Signal::SIGCHLD => {
                match proc.try_wait() {
                    Ok(Some(status)) if running => {
                        reap_zombies();
                        return exit_status_to_exit_code(status);
                    },
                    Ok(_) => {
                        reap_zombies();
                    },
                    Err(_) => {
                        return ExitCode::FAILURE;
                    }
                }
            },
            Signal::SIGUSR1 => {
                running = false;
                let _ = proc.kill();
            },
            Signal::SIGUSR2 => {
                running = true;
                let _ = proc.kill();
                sigset.thread_unblock().expect("Failed to unblock signals");
                proc = match spawn_child_process() {
                    Ok(child) => child,
                    Err(e) => {
                        eprintln!("Failed to start child process: {}", e);
                        return ExitCode::FAILURE;
                    },
                };
                sigset.thread_block().expect("Failed to block signals");
            },
            Signal::SIGINT | Signal::SIGTERM if !running => {
                return ExitCode::SUCCESS;
            },
            _ => {
                if let Ok(pid) = proc.id().try_into() {
                    let pid = Pid::from_raw(pid);
                    println!("Sending {:?} to {:?}", signal, pid);
                    let _ = signal::kill(pid, signal).expect("Error sending signal to process");
                }
            }
        }
    }

    ExitCode::SUCCESS
}