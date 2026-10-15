//! One local launch-pipe deadline across connect, header, body and acknowledgement.
//! No buffer or OVERLAPPED storage is released while Windows still owns an I/O.
use std::fs::File;
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, ERROR_IO_PENDING, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile, FILE_FLAG_OVERLAPPED};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

const TIMEOUT: Duration = Duration::from_secs(10);

fn timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "LAN configuration handoff timed out")
}

fn win_error(error: windows::core::Error) -> io::Error {
    // These calls report HRESULT_FROM_WIN32(GetLastError()).
    io::Error::from_raw_os_error(error.code().0 & 0xffff)
}

fn wait_millis(remaining: Duration) -> u32 {
    // Round nonzero waits up; never pass INFINITE by overflow or saturation.
    ((remaining.as_nanos().saturating_add(999_999) / 1_000_000).min(u128::from(u32::MAX - 1))) as u32
}

fn remaining_millis(deadline: Instant) -> io::Result<u32> {
    deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()).map(wait_millis).ok_or_else(timed_out)
}

struct Event(HANDLE);
impl Drop for Event {
    fn drop(&mut self) {
        unsafe { let _ = CloseHandle(self.0); }
    }
}

struct Pipe {
    file: File,
    event: Event,
}

impl Pipe {
    fn new(file: File) -> io::Result<Self> {
        let event = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.map_err(win_error)?;
        Ok(Self { file, event: Event(event) })
    }

    fn handle(&self) -> HANDLE {
        HANDLE(self.file.as_raw_handle() as isize)
    }

    fn transfer(&self, deadline: Instant, start: impl FnOnce(*mut OVERLAPPED) -> windows::core::Result<()>) -> io::Result<usize> {
        remaining_millis(deadline)?;
        let mut operation = OVERLAPPED { hEvent: self.event.0, ..OVERLAPPED::default() };
        let result = start(&mut operation);
        let pending = match result {
            Ok(()) => false,
            Err(error) if error.code() == ERROR_IO_PENDING.to_hresult() => true,
            Err(error) => return Err(win_error(error)),
        };
        if pending {
            let wait = match remaining_millis(deadline) {
                Ok(ms) => unsafe { WaitForSingleObject(self.event.0, ms) },
                Err(_) => WAIT_TIMEOUT,
            };
            if wait != WAIT_OBJECT_0 {
                let reason = if wait == WAIT_TIMEOUT { timed_out() } else { io::Error::last_os_error() };
                unsafe {
                    // Cancellation is a request, not completion. It can race
                    // normal completion; keep operation, event and buffer alive
                    // until GetOverlappedResult observes the terminal operation.
                    let _ = CancelIoEx(self.handle(), Some(&operation));
                    let mut completed = 0;
                    let _ = GetOverlappedResult(self.handle(), &operation, &mut completed, true);
                }
                return Err(reason);
            }
        }
        let mut completed = 0;
        unsafe { GetOverlappedResult(self.handle(), &operation, &mut completed, false) }.map_err(win_error)?;
        Ok(completed as usize)
    }

    fn read_exact(&self, mut buffer: &mut [u8], deadline: Instant) -> io::Result<()> {
        while !buffer.is_empty() {
            let count = self.transfer(deadline, |operation| unsafe { ReadFile(self.handle(), Some(buffer), None, Some(operation)) })?;
            if count == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "LAN configuration frame ended early"));
            }
            buffer = buffer.get_mut(count..).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Invalid LAN pipe read length"))?;
        }
        Ok(())
    }

    fn acknowledge(&self, deadline: Instant) -> io::Result<()> {
        let count = self.transfer(deadline, |operation| unsafe { WriteFile(self.handle(), Some(&[1]), None, Some(operation)) })?;
        if count != 1 {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "LAN configuration acknowledgement incomplete"));
        }
        Ok(())
    }
}

pub(super) fn receive(content: &str) -> anyhow::Result<String> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Reference { launch_pipe: String }
    let reference: Reference = toml::from_str(content)?;
    let suffix = reference.launch_pipe.strip_prefix("scbl-lan-").ok_or_else(|| anyhow::anyhow!("Invalid LAN launch pipe"))?;
    if suffix.len() != 32 || !suffix.bytes().all(|b| b.is_ascii_hexdigit()) {
        anyhow::bail!("Invalid LAN launch pipe");
    }
    let path = format!(r"\\.\pipe\{}", reference.launch_pipe);
    let deadline = Instant::now() + TIMEOUT;
    let file = loop {
        match std::fs::OpenOptions::new().read(true).write(true).custom_flags(FILE_FLAG_OVERLAPPED.0).open(&path) {
            Ok(file) => break file,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50).min(deadline.saturating_duration_since(Instant::now()))),
            Err(error) => return Err(error.into()),
        }
    };
    let pipe = Pipe::new(file)?;
    let mut length = [0u8; 4];
    pipe.read_exact(&mut length, deadline)?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > 65536 { anyhow::bail!("LAN config exceeds size limit"); }
    let mut bytes = vec![0u8; length];
    pipe.read_exact(&mut bytes, deadline)?;
    pipe.acknowledge(deadline)?;
    // Preserve the existing receipt-ACK protocol; parsing remains the caller's job.
    Ok(String::from_utf8(bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn waits_round_up_without_becoming_infinite() {
        assert_eq!(wait_millis(Duration::ZERO), 0);
        assert_eq!(wait_millis(Duration::from_nanos(1)), 1);
        assert_eq!(wait_millis(Duration::from_millis(10)), 10);
        assert_eq!(wait_millis(Duration::from_micros(10001)), 11);
        assert_eq!(wait_millis(Duration::MAX), u32::MAX - 1);
    }
    #[test]
    fn expired_deadline_is_a_timeout() {
        assert_eq!(remaining_millis(Instant::now() - Duration::from_secs(1)).unwrap_err().kind(), io::ErrorKind::TimedOut);
    }
    #[test]
    fn future_deadline_remains_finite() {
        let ms = remaining_millis(Instant::now() + TIMEOUT).unwrap();
        assert!(ms > 0 && ms <= 10000);
    }
    #[test]
    fn malformed_references_reject_before_opening_a_pipe() {
        for value in ["", "LaunchPipe = 123", "LaunchPipe = 'other-123'", "LaunchPipe = 'scbl-lan-short'", "LaunchPipe = 'scbl-lan-gggggggggggggggggggggggggggggggg'"] {
            assert!(receive(value).is_err());
        }
    }
}
