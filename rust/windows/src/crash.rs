//! Local minidumps for unhandled native exceptions and Rust panics.
#![warn(clippy::undocumented_unsafe_blocks)]

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, Read, Write},
    os::windows::{
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        OnceLock,
        atomic::{AtomicU32, Ordering},
    },
};
use windows::Win32::{
    Foundation::*,
    Storage::FileSystem::WriteFile,
    System::{
        Diagnostics::{
            Debug::*,
            ToolHelp::{
                CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First,
                Thread32Next,
            },
        },
        Threading::*,
    },
};

static REPORTER: OnceLock<Reporter> = OnceLock::new();
static WRITING: AtomicU32 = AtomicU32::new(0);

struct Reporter {
    child: Child,
    completed: OwnedHandle,
    directory: PathBuf,
}
impl Reporter {
    fn wait(&self) -> bool {
        // SAFETY: both handles are owned by `self.child` and `self.completed`, open while `self`
        // lives.
        unsafe {
            let process = HANDLE(self.child.as_raw_handle());
            if WaitForMultipleObjects(
                &[HANDLE(self.completed.as_raw_handle()), process],
                false,
                30_000,
            ) == WAIT_OBJECT_0
            {
                return true;
            }
            // The exception pointers cannot outlive the thread waiting here.
            let _ = TerminateProcess(process, 1);
            let _ = WaitForSingleObject(process, INFINITE);
            false
        }
    }
}
impl Drop for Reporter {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Serialize, Deserialize)]
struct Startup {
    parent: u32,
    completed: usize,
    directory: PathBuf,
}

fn dump(exceptions: *const EXCEPTION_POINTERS) -> bool {
    let Some(reporter) = REPORTER.get() else {
        return false;
    };
    // SAFETY: GetCurrentThreadId has no preconditions.
    let thread = unsafe { GetCurrentThreadId() };
    while let Err(owner) = WRITING.compare_exchange(0, thread, Ordering::Acquire, Ordering::Relaxed)
    {
        if owner == thread {
            return false;
        }
        // A second crashing thread must not exit the process during a report.
        // SAFETY: Sleep has no preconditions.
        unsafe { Sleep(1) };
    }
    // No allocation, DLL loading or stdio locks on the faulting thread. The
    // helper reads these pointers while this thread keeps their storage alive.
    let mut request = [0u8; 12];
    request[..4].copy_from_slice(&thread.to_le_bytes());
    request[4..].copy_from_slice(&(exceptions as u64).to_le_bytes());
    let mut written = 0;
    // SAFETY: the stdin pipe handle is owned by the reporter child, which lives in REPORTER for the
    // rest of the process, and `request` and `written` outlive the call.
    let result = unsafe {
        WriteFile(
            HANDLE(reporter.child.stdin.as_ref().unwrap().as_raw_handle()),
            Some(&request),
            Some(&mut written),
            None,
        )
        .is_ok()
            && written as usize == request.len()
            && reporter.wait()
    };
    WRITING.store(0, Ordering::Release);
    result
}
unsafe extern "system" fn unhandled(exceptions: *const EXCEPTION_POINTERS) -> i32 {
    dump(exceptions);
    1 // EXCEPTION_EXECUTE_HANDLER
}
pub fn initialize(directory: &Path) -> Result<()> {
    ensure!(
        REPORTER.get().is_none(),
        "crash reporting already initialized"
    );
    let directory = directory.join("crashes");
    std::fs::create_dir_all(&directory)?;
    // SAFETY: CreateEventW returns a new event handle that nothing else owns.
    let completed =
        unsafe { OwnedHandle::from_raw_handle(CreateEventW(None, false, false, None)?.0) };
    let mut command = Command::new(std::env::current_exe()?);
    #[cfg(not(test))]
    command.arg("--crash-reporter");
    #[cfg(test)]
    command.args(["--exact", "crash::tests::reporter_process", "--ignored"]);
    // Start before a crash: creating a process can itself need the loader or heap.
    let mut reporter = Reporter {
        child: command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW.0)
            .spawn()
            .context("starting crash reporter")?,
        completed,
        directory,
    };
    let mut remote = HANDLE::default();
    // SAFETY: both source handles are owned by `reporter`, and `remote` is a valid out-pointer.
    unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            HANDLE(reporter.completed.as_raw_handle()),
            HANDLE(reporter.child.as_raw_handle()),
            &mut remote,
            0,
            false,
            DUPLICATE_SAME_ACCESS,
        )?;
    }
    let startup = Startup {
        parent: std::process::id(),
        completed: remote.0 as usize,
        directory: reporter.directory.clone(),
    };
    let input = reporter.child.stdin.as_mut().unwrap();
    serde_json::to_writer(&mut *input, &startup)?;
    input.write_all(b"\n")?;
    ensure!(reporter.wait(), "crash reporter did not initialize");
    REPORTER
        .set(reporter)
        .map_err(|_| anyhow::anyhow!("crash reporting already initialized"))?;
    // SAFETY: `unhandled` has the signature the filter expects and stays valid for the whole
    // process.
    unsafe {
        SetUnhandledExceptionFilter(Some(unhandled));
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(reporter) = REPORTER.get() {
            let _ = std::fs::write(reporter.directory.join("panic.txt"), info.to_string());
        }
        dump(std::ptr::null());
        previous(info);
    }));
    Ok(())
}

/// Private helper entry point; stdin and the completion event belong to the host.
pub fn reporter() -> Result<()> {
    let mut input = std::io::stdin().lock();
    let mut line = String::new();
    input.read_line(&mut line)?;
    let startup: Startup = serde_json::from_str(&line)?;
    // SAFETY: the host duplicated this event into this process for the helper alone, so it is ours
    // to own.
    let completed = unsafe { OwnedHandle::from_raw_handle(startup.completed as *mut _) };
    // SAFETY: OpenProcess returns a new process handle that nothing else owns.
    let process = unsafe {
        OwnedHandle::from_raw_handle(
            OpenProcess(
                PROCESS_QUERY_INFORMATION | PROCESS_VM_READ,
                false,
                startup.parent,
            )?
            .0,
        )
    };
    // SAFETY: `completed` is an owned event handle, open for the call.
    unsafe { SetEvent(HANDLE(completed.as_raw_handle()))? };
    loop {
        let mut request = [0u8; 12];
        if let Err(error) = input.read_exact(&mut request) {
            if error.kind() == std::io::ErrorKind::UnexpectedEof {
                return Ok(());
            }
            return Err(error.into());
        }
        let thread = u32::from_le_bytes(request[..4].try_into().unwrap());
        let exceptions = u64::from_le_bytes(request[4..].try_into().unwrap());
        let info = (exceptions != 0).then_some(MINIDUMP_EXCEPTION_INFORMATION {
            ThreadId: thread,
            ExceptionPointers: exceptions as *mut EXCEPTION_POINTERS,
            ClientPointers: true.into(),
        });
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis();
        let path = startup
            .directory
            .join(format!("butterpollo.{}.{stamp}.dmp", startup.parent));
        let mut result = Ok(());
        for attempt in 1..=3 {
            result = write(&process, startup.parent, thread, info.as_ref(), &path)
                .with_context(|| format!("writing {} (attempt {attempt})", path.display()));
            if result.is_ok() {
                break;
            }
        }
        if let Err(error) = &result {
            // An empty dump says nothing; keep why it failed instead.
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::write(path.with_extension("error.txt"), format!("{error:#}"));
        }
        result?;
        // SAFETY: `completed` is an owned event handle, open for the call.
        unsafe { SetEvent(HANDLE(completed.as_raw_handle()))? };
    }
}

fn write(
    process: &OwnedHandle,
    id: u32,
    thread: u32,
    info: Option<&MINIDUMP_EXCEPTION_INFORMATION>,
    path: &Path,
) -> Result<()> {
    let file = std::fs::File::create(path)?;
    {
        let _still = Suspended::threads(id, thread);
        // SAFETY: `process` and `file` are owned open handles, and `info` outlives the call and
        // points into the host, read with ClientPointers set.
        unsafe {
            // DbgHelp must never suspend threads in its own process: one of
            // them may own a loader/heap lock that the dump writer needs.
            MiniDumpWriteDump(
                HANDLE(process.as_raw_handle()),
                id,
                HANDLE(file.as_raw_handle()),
                MiniDumpNormal | MiniDumpWithThreadInfo | MiniDumpWithUnloadedModules,
                info.map(|i| i as *const _),
                None,
                None,
            )?;
        }
    }
    file.sync_all()?;
    Ok(())
}

/// The host's threads, held still while DbgHelp reads them: a thread that exits or frees memory
/// mid-dump fails MiniDumpWriteDump and leaves an empty file. The reporting thread is left
/// running so its timeout still works if this helper hangs.
struct Suspended(Vec<OwnedHandle>);
impl Suspended {
    fn threads(process: u32, except: u32) -> Self {
        let mut threads = Vec::new();
        // SAFETY: the snapshot and thread handles are new handles owned here, `entry` outlives
        // each call, and each thread is suspended once and resumed once on drop.
        unsafe {
            let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) else {
                return Self(threads);
            };
            let snapshot = OwnedHandle::from_raw_handle(snapshot.0);
            let mut entry = THREADENTRY32 {
                dwSize: size_of::<THREADENTRY32>() as u32,
                ..Default::default()
            };
            let mut found = Thread32First(HANDLE(snapshot.as_raw_handle()), &mut entry);
            while found.is_ok() {
                if entry.th32OwnerProcessID == process
                    && entry.th32ThreadID != except
                    && let Ok(handle) = OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID)
                {
                    let handle = OwnedHandle::from_raw_handle(handle.0);
                    if SuspendThread(HANDLE(handle.as_raw_handle())) != u32::MAX {
                        threads.push(handle);
                    }
                }
                found = Thread32Next(HANDLE(snapshot.as_raw_handle()), &mut entry);
            }
        }
        Self(threads)
    }
}
impl Drop for Suspended {
    fn drop(&mut self) {
        for thread in &self.0 {
            // SAFETY: each handle is an owned thread handle this helper suspended once.
            unsafe { ResumeThread(HANDLE(thread.as_raw_handle())) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    #[ignore = "subprocess entry point"]
    fn reporter_process() {
        std::process::exit(if reporter().is_ok() { 0 } else { 1 });
    }

    #[test]
    #[ignore = "subprocess entry point"]
    fn crashing_process() {
        let directory = PathBuf::from(std::env::var_os("BUTTERPOLLO_CRASH_TEST_DIR").unwrap());
        // SAFETY: SetErrorMode only changes this process's error mode.
        unsafe { SetErrorMode(SEM_NOGPFAULTERRORBOX) };
        initialize(&directory).unwrap();
        std::fs::write(
            directory.join("thread.txt"),
            // SAFETY: GetCurrentThreadId has no preconditions.
            unsafe { GetCurrentThreadId() }.to_string(),
        )
        .unwrap();
        match std::env::var("BUTTERPOLLO_CRASH_TEST").unwrap().as_str() {
            // SAFETY: `Some(&[0x1234, 0x5678])` lives for the call and RaiseException copies it.
            "native" => unsafe { RaiseException(0xe042_5050, 1, Some(&[0x1234, 0x5678])) },
            "panic" => panic!("crash report regression"),
            "heap" => {
                use windows::Win32::System::Memory::*;
                // SAFETY: CreateEventW returns a new event handle that nothing else owns.
                let ready = unsafe {
                    OwnedHandle::from_raw_handle(CreateEventW(None, false, false, None).unwrap().0)
                };
                // SAFETY: CreateEventW returns a new event handle that nothing else owns.
                let release = unsafe {
                    OwnedHandle::from_raw_handle(CreateEventW(None, false, false, None).unwrap().0)
                };
                let ready_worker = ready.try_clone().unwrap();
                let release_worker = release.try_clone().unwrap();
                // SAFETY: the heap handle is the process heap and is unlocked once, and both event
                // handles are clones owned by the closure.
                let thread = std::thread::spawn(move || unsafe {
                    let heap = GetProcessHeap().unwrap();
                    HeapLock(heap).unwrap();
                    SetEvent(HANDLE(ready_worker.as_raw_handle())).unwrap();
                    WaitForSingleObject(HANDLE(release_worker.as_raw_handle()), INFINITE);
                    HeapUnlock(heap).unwrap();
                });
                // SAFETY: `ready` is an owned event handle, open for the wait.
                unsafe { WaitForSingleObject(HANDLE(ready.as_raw_handle()), INFINITE) };
                let written = dump(std::ptr::null());
                // SAFETY: `release` is an owned event handle, open for the call.
                unsafe { SetEvent(HANDLE(release.as_raw_handle())).unwrap() };
                thread.join().unwrap();
                assert!(written);
            }
            mode => panic!("unknown crash test {mode}"),
        }
    }

    fn report(mode: &str) -> (tempfile::TempDir, Vec<u8>) {
        let directory = tempfile::tempdir().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "crash::tests::crashing_process", "--ignored"])
            .env("BUTTERPOLLO_CRASH_TEST_DIR", directory.path())
            .env("BUTTERPOLLO_CRASH_TEST", mode)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW.0)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(40);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("{mode} crash report timed out");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(
            status.code().unwrap() as u32,
            match mode {
                "native" => 0xe042_5050,
                "panic" => 101,
                "heap" => 0,
                _ => unreachable!(),
            }
        );
        let files: Vec<_> = std::fs::read_dir(directory.path().join("crashes"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        let reports: Vec<_> = files
            .iter()
            .filter(|path| path.extension().is_some_and(|ext| ext == "dmp"))
            .collect();
        let errors: Vec<_> = files
            .iter()
            .filter(|path| path.to_string_lossy().ends_with(".error.txt"))
            .map(|path| std::fs::read_to_string(path).unwrap())
            .collect();
        assert_eq!(reports.len(), 1, "reporter errors: {errors:?}");
        let bytes = std::fs::read(reports[0]).unwrap();
        assert_eq!(&bytes[..4], b"MDMP");
        assert!(bytes.len() > 32);
        (directory, bytes)
    }

    fn read<T: Copy>(bytes: &[u8], offset: u32) -> T {
        let bytes = &bytes[offset as usize..][..size_of::<T>()];
        // SAFETY: the slice above is exactly size_of::<T>() bytes, and callers only read plain-data
        // minidump structs, which are valid for any bit pattern.
        unsafe { bytes.as_ptr().cast::<T>().read_unaligned() }
    }

    #[test]
    fn native_report_has_the_windows_minidump_signature() {
        let (directory, bytes) = report("native");
        let header: MINIDUMP_HEADER = read(&bytes, 0);
        let exception = (0..header.NumberOfStreams)
            .map(|index| {
                read::<MINIDUMP_DIRECTORY>(
                    &bytes,
                    header.StreamDirectoryRva + index * size_of::<MINIDUMP_DIRECTORY>() as u32,
                )
            })
            .find(|stream| stream.StreamType == ExceptionStream.0 as u32)
            .unwrap();
        let exception: MINIDUMP_EXCEPTION_STREAM = read(&bytes, exception.Location.Rva);
        let thread = std::fs::read_to_string(directory.path().join("thread.txt"))
            .unwrap()
            .parse::<u32>()
            .unwrap();
        assert_eq!({ exception.ThreadId }, thread);
        assert_eq!({ exception.ExceptionRecord.ExceptionCode }, 0xe042_5050);
        assert_eq!({ exception.ExceptionRecord.NumberParameters }, 2);
        assert_eq!(
            &{ exception.ExceptionRecord.ExceptionInformation }[..2],
            &[0x1234, 0x5678]
        );
        let context: CONTEXT = read(&bytes, exception.ThreadContext.Rva);
        assert_ne!(context.Rip, 0);
        assert_eq!({ exception.ExceptionRecord.ExceptionAddress }, context.Rip);
    }

    #[test]
    fn panic_still_produces_a_report() {
        let (directory, _) = report("panic");
        let panic = std::fs::read_to_string(directory.path().join("crashes/panic.txt")).unwrap();
        assert!(panic.contains("crash report regression"));
    }

    #[test]
    fn report_does_not_need_the_target_process_heap() {
        report("heap");
    }
}
