// Samples a thread as a sampling profiler does: it suspends the thread, reads its stack pointer and copies the stack
// from there up to the stack's top. A check of cranvr's Wine build: on macOS, a thread stack must not lie right above
// the stack that Wine's Unix side uses for the thread's system calls (patch 0005).
//
// Wine can report a stack pointer of its Unix side. A thread that is suspended on its way out of a system call
// (__wine_syscall_dispatcher_return) gets a context whose Rsp points into the syscall frame on the Unix-side stack.
// On macOS that window includes a system call of its own. Chromium's sampling profiler, which Steam's web helper runs,
// copies the stack from that Rsp to the thread's StackBase. The Unix-side stacks of Wine lie above 4 GB. With patch
// 0005 and thread stacks above 4 GB too, a thread stack started right after the Unix-side stack of its thread. The
// copy then ran into the no-access page at the bottom of the thread stack. The web helper crashed every few
// seconds. A reported Rsp above StackBase is harmless: the profiler drops the sample.
//
// The program runs a thread that makes system calls in a loop. It suspends the thread many times, reads Rsp and
// checks the range that a profiler copies, as long as the range is short. A page in it that the copy cannot read fails
// the check.
//
// Usage: stack_sample_check.exe
// Prints "stack_sample_check: PASS" or "stack_sample_check: FAIL" with the counts.

#include <windows.h>
#include <cstdint>
#include <cstdio>

namespace {

const int samples = 20000;
const uintptr_t short_copy = 8 * 1024 * 1024;  // a profiler copies at most this much (its buffer)

volatile LONG stop;
volatile uintptr_t stack_base, stack_bottom;

DWORD WINAPI busy(void *)
{
    NT_TIB *tib = reinterpret_cast<NT_TIB *>(NtCurrentTeb());
    stack_base = reinterpret_cast<uintptr_t>(tib->StackBase);
    // TEB.DeallocationStack: the lowest address of the thread's stack
    stack_bottom = *reinterpret_cast<uintptr_t *>(reinterpret_cast<char *>(NtCurrentTeb()) + 0x1478);
    while (!stop) SwitchToThread();
    return 0;
}

// Whether a profiler's copy of the range would fault: a page of it is not committed or not readable
bool copy_faults(uintptr_t from, uintptr_t to)
{
    for (uintptr_t at = from; at < to;)
    {
        MEMORY_BASIC_INFORMATION info;
        if (!VirtualQuery(reinterpret_cast<void *>(at), &info, sizeof(info))) return true;
        if (info.State != MEM_COMMIT || (info.Protect & (PAGE_NOACCESS | PAGE_GUARD))) return true;
        at = reinterpret_cast<uintptr_t>(info.BaseAddress) + info.RegionSize;
    }
    return false;
}

}  // namespace

int main()
{
    HANDLE thread = CreateThread(nullptr, 0, busy, nullptr, 0, nullptr);
    if (!thread) return std::printf("stack_sample_check: FAIL no thread\n"), 1;
    while (!stack_base) Sleep(1);
    std::printf("stack_sample_check: thread stack %#llx-%#llx\n", static_cast<unsigned long long>(stack_bottom),
                static_cast<unsigned long long>(stack_base));

    int inside = 0, above = 0, below = 0, faults = 0;
    uintptr_t first_fault_rsp = 0;
    for (int i = 0; i < samples; i++)
    {
        if (SuspendThread(thread) == static_cast<DWORD>(-1)) return std::printf("stack_sample_check: FAIL no suspend\n"), 1;
        CONTEXT context = {};
        context.ContextFlags = CONTEXT_CONTROL;
        if (GetThreadContext(thread, &context))
        {
            uintptr_t rsp = context.Rsp;
            if (rsp >= stack_bottom && rsp < stack_base) inside++;
            else if (rsp >= stack_base) above++;
            else
            {
                below++;
                if (stack_base - rsp <= short_copy && copy_faults(rsp, stack_base))
                {
                    if (!faults) first_fault_rsp = rsp;
                    faults++;
                }
            }
        }
        ResumeThread(thread);
    }
    stop = 1;
    WaitForSingleObject(thread, INFINITE);

    std::printf("stack_sample_check: %d samples: %d in the stack, %d above it, %d below it\n", samples, inside, above,
                below);
    if (faults)
    {
        std::printf("stack_sample_check: FAIL %d copies would fault; the first from Rsp %#llx\n", faults,
                    static_cast<unsigned long long>(first_fault_rsp));
        return 1;
    }
    std::printf("stack_sample_check: PASS\n");
    return 0;
}
