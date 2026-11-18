//! Thread priorities: the audio callback first, background work last.
//!
//! A DJ program does three kinds of work at once: the audio callback (a few hundred microseconds, hundreds of times a
//! second, and a late one is a click), the screen, and long jobs nobody is waiting on (analysing the library, splitting
//! stems). Left alone the operating system treats them alike, so six analysis threads can take the processor from the
//! callback. Here the long jobs ask to run only when nothing else wants the processor, and the callback asks for
//! real-time scheduling where the system allows it.
//!
//! Linux and macOS only; elsewhere these do nothing. Not measured on macOS or Windows.

use std::sync::atomic::{AtomicU8, Ordering};

/// How the audio thread ended up scheduled: 0 not asked yet, 1 real-time, 2 raised priority, 3 neither was allowed.
static AUDIO: AtomicU8 = AtomicU8::new(0);

/// What [`boost_audio_thread`] got, for the log: `None` until the first audio block.
pub fn audio_thread_status() -> Option<&'static str> {
    match AUDIO.load(Ordering::Relaxed) {
        1 => Some("real-time"),
        2 => Some("raised priority"),
        3 => Some("normal priority (the system allows neither real-time nor a raised priority for this user)"),
        _ => None,
    }
}

/// Makes the calling thread a background one: it runs when the audio, the screen and loading a track onto a deck do not
/// need the processor. Threads it starts afterwards inherit this on Linux. Call it first thing in a worker.
pub fn background() {
    #[cfg(target_os = "linux")]
    unsafe {
        // on Linux a thread has its own nice value, and "process 0" here is the calling thread
        libc::setpriority(libc::PRIO_PROCESS, 0, 15);
    }
    #[cfg(target_os = "macos")]
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
    }
}

/// Asks for the calling thread (the audio callback's) to be scheduled ahead of everything ordinary: real-time if the
/// user may have it, else a raised priority, else nothing. Safe to call from the callback: two system calls, once.
pub fn boost_audio_thread() {
    if AUDIO.load(Ordering::Relaxed) != 0 {
        return;
    }
    #[cfg(target_os = "linux")]
    let got = unsafe {
        let param = libc::sched_param { sched_priority: 10 };
        if libc::sched_setscheduler(0, libc::SCHED_FIFO | libc::SCHED_RESET_ON_FORK, &param) == 0 {
            1
        } else if libc::setpriority(libc::PRIO_PROCESS, 0, -11) == 0 {
            2
        } else {
            3
        }
    };
    // CoreAudio and WASAPI run their callbacks on threads the system already schedules for audio
    #[cfg(not(target_os = "linux"))]
    let got = 1;
    AUDIO.store(got, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_background_thread_still_runs_and_asking_twice_is_harmless() {
        let h = std::thread::spawn(|| {
            background();
            background();
            (0..1000u64).sum::<u64>()
        });
        assert_eq!(h.join().unwrap(), 499_500);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_background_thread_is_nicer_than_the_one_that_started_it() {
        let nice = || unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) };
        let here = nice();
        let there = std::thread::spawn(move || {
            background();
            nice()
        })
        .join()
        .unwrap();
        assert!(there > here, "{there} against {here}");
        assert_eq!(nice(), here, "and the caller is untouched");
    }
}
