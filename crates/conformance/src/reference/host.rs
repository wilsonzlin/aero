use crate::corpus::TestCase;
use crate::{CpuState, ExecOutcome, Fault, FLAG_FIXED_1};
use core::arch::global_asm;
use std::io::Read;
use std::mem::size_of;
use std::os::unix::io::{FromRawFd, RawFd};

pub struct ReferenceBackend {
    code: ExecutablePage,
    memory: GuardedMemory,
    isolate: bool,
}

impl ReferenceBackend {
    pub fn new() -> Result<Self, &'static str> {
        let isolate = std::env::var("AERO_CONFORMANCE_REFERENCE_ISOLATE")
            .map(|v| v != "0")
            .unwrap_or(true);

        Ok(Self {
            code: ExecutablePage::new()?,
            memory: GuardedMemory::new()?,
            isolate,
        })
    }

    pub fn memory_base(&self) -> u64 {
        self.memory.base as u64
    }

    pub fn execute(&mut self, case: &TestCase) -> ExecOutcome {
        if self.isolate {
            return self.execute_isolated(case);
        }

        let mut input = case.init;
        input.rflags |= FLAG_FIXED_1;
        let mut output = CpuState::default();
        output.rflags = FLAG_FIXED_1;

        self.memory.write(&case.memory);

        unsafe {
            self.code.write(case.template.bytes);
            aero_conformance_host_exec(self.code.ptr, &input, &mut output);
        }

        output.rip = input.rip.wrapping_add(case.template.bytes.len() as u64);

        ExecOutcome {
            state: output,
            memory: self.memory.read(case.memory.len()),
            fault: None,
        }
    }

    fn execute_isolated(&mut self, case: &TestCase) -> ExecOutcome {
        let mut input = case.init;
        input.rflags |= FLAG_FIXED_1;

        self.memory.write(&case.memory);

        let mut pipe_fds = [0i32; 2];
        let rc = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
        if rc != 0 {
            return ExecOutcome {
                state: CpuState::default(),
                memory: Vec::new(),
                fault: Some(Fault::Unsupported("pipe() failed")),
            };
        }

        unsafe {
            self.code.write(case.template.bytes);
        }

        let pid = unsafe { libc::fork() };
        if pid < 0 {
            unsafe {
                libc::close(pipe_fds[0]);
                libc::close(pipe_fds[1]);
            }
            return ExecOutcome {
                state: CpuState::default(),
                memory: Vec::new(),
                fault: Some(Fault::Unsupported("fork() failed")),
            };
        }

        if pid == 0 {
            unsafe {
                libc::close(pipe_fds[0]);
            }

            let mut output = CpuState::default();
            output.rflags = FLAG_FIXED_1;
            unsafe {
                aero_conformance_host_exec(self.code.ptr, &input, &mut output);
            }
            output.rip = input.rip.wrapping_add(case.template.bytes.len() as u64);

            unsafe {
                let state_ptr = (&output as *const CpuState) as *const u8;
                write_all(pipe_fds[1], state_ptr, size_of::<CpuState>());
                write_all(
                    pipe_fds[1],
                    self.memory.base as *const u8,
                    case.memory.len(),
                );
                libc::close(pipe_fds[1]);
                libc::_exit(0);
            }
        }

        unsafe {
            libc::close(pipe_fds[1]);
        }

        let status = wait_for_child(pid);
        if let Some(fault) = status {
            unsafe {
                libc::close(pipe_fds[0]);
            }
            return ExecOutcome {
                state: CpuState::default(),
                memory: Vec::new(),
                fault: Some(fault),
            };
        }

        let mut file = unsafe { std::fs::File::from_raw_fd(pipe_fds[0]) };
        let state_size = size_of::<CpuState>();
        let mut buffer = vec![0u8; state_size + case.memory.len()];

        if let Err(_) = file.read_exact(&mut buffer) {
            return ExecOutcome {
                state: CpuState::default(),
                memory: Vec::new(),
                fault: Some(Fault::Unsupported("reference runner output truncated")),
            };
        }

        let mut output = CpuState::default();
        unsafe {
            core::ptr::copy_nonoverlapping(
                buffer.as_ptr(),
                (&mut output as *mut CpuState) as *mut u8,
                state_size,
            );
        }
        let memory = buffer[state_size..].to_vec();

        ExecOutcome {
            state: output,
            memory,
            fault: None,
        }
    }
}

fn wait_for_child(pid: libc::pid_t) -> Option<Fault> {
    let mut status: i32 = 0;
    let rc = unsafe { libc::waitpid(pid, &mut status, 0) };
    if rc < 0 {
        return Some(Fault::Unsupported("waitpid() failed"));
    }

    if libc::WIFSIGNALED(status) {
        let sig = libc::WTERMSIG(status);
        return Some(Fault::Signal(sig));
    }

    if libc::WIFEXITED(status) {
        let code = libc::WEXITSTATUS(status);
        if code != 0 {
            return Some(Fault::Unsupported("reference runner exited non-zero"));
        }
    }

    None
}

unsafe fn write_all(fd: RawFd, mut ptr: *const u8, mut len: usize) {
    while len > 0 {
        let rc = libc::write(fd, ptr as *const libc::c_void, len);
        if rc <= 0 {
            libc::_exit(111);
        }
        let written = rc as usize;
        ptr = ptr.add(written);
        len -= written;
    }
}

extern "C" {
    fn aero_conformance_host_exec(code: *const u8, init: *const CpuState, out: *mut CpuState);
}

global_asm!(
    r#"
    .global aero_conformance_host_exec
    .type aero_conformance_host_exec, @function
aero_conformance_host_exec:
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15

    sub rsp, 24
    mov QWORD PTR [rsp+0], rdi
    mov QWORD PTR [rsp+8], rsi
    mov QWORD PTR [rsp+16], rdx

    mov r11, QWORD PTR [rsp+8]
    mov rax, QWORD PTR [r11+0]
    mov rbx, QWORD PTR [r11+8]
    mov rcx, QWORD PTR [r11+16]
    mov rdx, QWORD PTR [r11+24]
    mov rsi, QWORD PTR [r11+32]
    mov rdi, QWORD PTR [r11+40]
    mov r8,  QWORD PTR [r11+48]
    mov r9,  QWORD PTR [r11+56]
    mov r10, QWORD PTR [r11+64]
    mov rbp, QWORD PTR [r11+72]
    mov r12, QWORD PTR [r11+80]
    mov r13, QWORD PTR [r11+88]
    mov r14, QWORD PTR [r11+96]
    mov r15, QWORD PTR [r11+104]
    push QWORD PTR [r11+112]
    popfq
    mov r11, rbp

    call QWORD PTR [rsp+0]

    mov rbp, QWORD PTR [rsp+16]
    mov QWORD PTR [rbp+0], rax
    mov QWORD PTR [rbp+8], rbx
    mov QWORD PTR [rbp+16], rcx
    mov QWORD PTR [rbp+24], rdx
    mov QWORD PTR [rbp+32], rsi
    mov QWORD PTR [rbp+40], rdi
    mov QWORD PTR [rbp+48], r8
    mov QWORD PTR [rbp+56], r9
    mov QWORD PTR [rbp+64], r10
    mov QWORD PTR [rbp+72], r11
    mov QWORD PTR [rbp+80], r12
    mov QWORD PTR [rbp+88], r13
    mov QWORD PTR [rbp+96], r14
    mov QWORD PTR [rbp+104], r15
    pushfq
    pop QWORD PTR [rbp+112]

    add rsp, 24

    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret
    .size aero_conformance_host_exec, .-aero_conformance_host_exec
    "#
);

struct ExecutablePage {
    ptr: *const u8,
    len: usize,
}

impl ExecutablePage {
    fn new() -> Result<Self, &'static str> {
        let page_size = page_size()?;
        let ptr = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                page_size,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };

        if ptr == libc::MAP_FAILED {
            return Err("mmap for code page failed");
        }

        Ok(Self {
            ptr: ptr as *const u8,
            len: page_size,
        })
    }

    unsafe fn write(&mut self, instruction: &[u8]) {
        let dst = self.ptr as *mut u8;
        core::ptr::copy_nonoverlapping(instruction.as_ptr(), dst, instruction.len());
        *dst.add(instruction.len()) = 0xC3;
    }
}

impl Drop for ExecutablePage {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.ptr as *mut libc::c_void, self.len);
        }
    }
}

struct GuardedMemory {
    mapping: *mut u8,
    len: usize,
    base: *mut u8,
}

impl GuardedMemory {
    fn new() -> Result<Self, &'static str> {
        let page_size = page_size()?;
        let len = page_size * 3;

        let mapping = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                len,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        if mapping == libc::MAP_FAILED {
            return Err("mmap for guarded memory failed");
        }

        let base = unsafe { (mapping as *mut u8).add(page_size) };
        let rc = unsafe {
            libc::mprotect(
                base as *mut libc::c_void,
                page_size,
                libc::PROT_READ | libc::PROT_WRITE,
            )
        };
        if rc != 0 {
            unsafe {
                libc::munmap(mapping, len);
            }
            return Err("mprotect for guarded memory failed");
        }

        Ok(Self {
            mapping: mapping as *mut u8,
            len,
            base,
        })
    }

    fn write(&mut self, bytes: &[u8]) {
        unsafe {
            core::ptr::write_bytes(self.base, 0, self.page_len());
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), self.base, bytes.len());
        }
    }

    fn read(&self, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        unsafe {
            core::ptr::copy_nonoverlapping(self.base, out.as_mut_ptr(), len);
        }
        out
    }

    fn page_len(&self) -> usize {
        self.len / 3
    }
}

impl Drop for GuardedMemory {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.mapping as *mut libc::c_void, self.len);
        }
    }
}

fn page_size() -> Result<usize, &'static str> {
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if size <= 0 {
        return Err("sysconf(_SC_PAGESIZE) failed");
    }
    Ok(size as usize)
}
