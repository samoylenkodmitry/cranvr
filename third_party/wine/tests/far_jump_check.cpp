// Follows the far jump that MonoMod.Core 1.3.3 writes, the detour library under Harmony in BSIPA's Beat Saber mods. A
// check of cranvr's Wine build: on macOS, a program that asks for high-entropy addresses must get its memory above
// 4 GB (patch 0005).
//
// MonoMod detours a method with `jmp qword ptr [rip+disp32]` when the target lies more than 2 GB from the jump. The
// target sits in a cell of 8 bytes within 2 GB of the jump. MonoMod finds a page for the cell with VirtualQuery: it
// walks from the jump down and up, nearest page first. It allocates the first free page that it finds. Version 1.3.3
// counts page 0 as free. VirtualAlloc at address 0 places the cell anywhere. A cell more than 2 GB away wraps the
// 32-bit displacement, so the jump reads another address. The detoured method then throws a NullReferenceException.
// In Beat Saber with BeatSaberMarkupLanguage, every text of the menus threw it and the buttons showed no labels.
//
// The walk reaches page 0 only from code below 2 GB. Windows places the memory of a program with high-entropy
// addresses above 4 GB. Wine on macOS placed it at the bottom of the address space, where Unity's reservations of
// 512 MB fill the rest.
//
// The program builds that case. It allocates code as MonoMod's runtime does, reserves every free page within 2 GB of
// it, allocates a target beyond, finds the cell with MonoMod's walk, writes the jump and calls it.
//
// Usage: far_jump_check.exe
// Prints "far_jump_check: PASS" or "far_jump_check: FAIL" with the reason.

#include <windows.h>
#include <cstdint>
#include <cstdio>

namespace {

const uintptr_t page_size = 0x10000;  // MonoMod's page: the allocation granularity
const intptr_t reach = 0x7fffffff;    // a 32-bit displacement reaches this far

volatile uintptr_t fault_address;
volatile bool faulted;
uint8_t *jump;

// The jump through a cell that the process cannot read raises an access violation at the jump. The handler returns
// to the caller of the jump with a mark instead, as MonoMod's runtime turns it into an exception.
LONG CALLBACK on_fault(EXCEPTION_POINTERS *info)
{
    if (info->ExceptionRecord->ExceptionCode != EXCEPTION_ACCESS_VIOLATION) return EXCEPTION_CONTINUE_SEARCH;
    if (info->ContextRecord->Rip != reinterpret_cast<DWORD64>(jump)) return EXCEPTION_CONTINUE_SEARCH;
    faulted = true;
    fault_address = info->ExceptionRecord->NumberParameters > 1 ? info->ExceptionRecord->ExceptionInformation[1] : 0;
    CONTEXT *context = info->ContextRecord;
    context->Rip = *reinterpret_cast<DWORD64 *>(context->Rsp);
    context->Rsp += 8;
    context->Rax = 0xbad;
    return EXCEPTION_CONTINUE_EXECUTION;
}

int fail(const char *what)
{
    std::printf("far_jump_check: FAIL %s\n", what);
    return 1;
}

// MonoMod's TryQueryPage: whether the page is free, and the block that holds it
bool query(uintptr_t page, bool &free, uintptr_t &base, uintptr_t &size)
{
    MEMORY_BASIC_INFORMATION info;
    if (!VirtualQuery(reinterpret_cast<void *>(page), &info, sizeof(info))) return false;
    free = info.State == MEM_FREE;
    base = free ? reinterpret_cast<uintptr_t>(info.BaseAddress) : reinterpret_cast<uintptr_t>(info.AllocationBase);
    size = page + info.RegionSize - base;
    return true;
}

// One step of MonoMod 1.3.3's walk (QueryingPagedMemoryAllocator.TryAllocNewPage), with its checks as they are
bool try_page(uintptr_t &page, bool going_up, uintptr_t &cell, bool &took_page_0)
{
    bool free;
    uintptr_t base, size;
    if (!query(page, free, base, size))
    {
        page = going_up ? page + page_size : page - page_size;
        return false;
    }
    if (free)
    {
        uintptr_t candidate = (page + page_size - 1) & ~(page_size - 1);
        if (candidate + page_size <= base + size)
        {
            void *got = VirtualAlloc(reinterpret_cast<void *>(candidate), page_size, MEM_RESERVE | MEM_COMMIT,
                                     PAGE_READWRITE);
            if (got)
            {
                took_page_0 = candidate == 0;
                cell = reinterpret_cast<uintptr_t>(got);
                return true;
            }
        }
    }
    page = going_up ? base + size : base - page_size;
    return false;
}

// MonoMod's TryAllocateNewPage for a positioned request: the nearest free page in [low, high)
bool find_cell(uintptr_t target, uintptr_t low, uintptr_t high, uintptr_t &cell, bool &took_page_0)
{
    uintptr_t low_page = target & ~(page_size - 1);
    uintptr_t high_page = low_page + page_size;
    // The page index is signed in MonoMod: a walk below 0 ends
    auto low_ok = [&] { return static_cast<intptr_t>(low_page) >= static_cast<intptr_t>(low); };
    while (low_ok() || high_page < high)
    {
        while (high_page < high && (!low_ok() || target - low_page >= high_page - target))
            if (try_page(high_page, true, cell, took_page_0)) return true;
        while (low_ok() && (high_page >= high || target - low_page < high_page - target))
            if (try_page(low_page, false, cell, took_page_0)) return true;
    }
    return false;
}

// Reserves every free block in [start, end), as the reservations of a running game fill the space near its code
void crowd(uintptr_t start, uintptr_t end)
{
    uintptr_t at = start;
    while (at < end)
    {
        MEMORY_BASIC_INFORMATION info;
        if (!VirtualQuery(reinterpret_cast<void *>(at), &info, sizeof(info))) break;
        uintptr_t block_end = reinterpret_cast<uintptr_t>(info.BaseAddress) + info.RegionSize;
        if (block_end > end) block_end = end;
        if (info.State == MEM_FREE)
        {
            uintptr_t first = (at + page_size - 1) & ~(page_size - 1);
            if (first < block_end)
                VirtualAlloc(reinterpret_cast<void *>(first), block_end - first, MEM_RESERVE, PAGE_NOACCESS);
        }
        if (block_end <= at) break;
        at = block_end;
    }
}

}  // namespace

int main()
{
    // MonoMod's runtime takes code memory anywhere, as Mono does for a dynamic method
    auto *code = static_cast<uint8_t *>(VirtualAlloc(nullptr, page_size, MEM_RESERVE | MEM_COMMIT,
                                                     PAGE_EXECUTE_READWRITE));
    if (!code) return fail("no code page");
    jump = code;
    uintptr_t from = reinterpret_cast<uintptr_t>(code);
    std::printf("far_jump_check: code at %#llx\n", static_cast<unsigned long long>(from));

    // ComputeDetourInfo's range for the cell, from the end of the 6-byte jump
    uintptr_t next = from + 6;
    uintptr_t low = next >= static_cast<uintptr_t>(reach) + 1 ? next - reach - 1 : 0;
    uintptr_t high = next + reach;

    // Nothing free is left near the code. The target lies beyond the reach of a 32-bit displacement.
    crowd(low > page_size ? low : page_size, high);
    auto *target = static_cast<uint8_t *>(VirtualAlloc(nullptr, page_size, MEM_RESERVE | MEM_COMMIT,
                                                       PAGE_EXECUTE_READWRITE));
    if (!target) return fail("no target page");
    uintptr_t to = reinterpret_cast<uintptr_t>(target);
    if (to >= low && to < high) return fail("the target lies within 2 GB of the code; the case was not built");
    const uint8_t mov_eax_42_ret[] = {0xb8, 0x2a, 0x00, 0x00, 0x00, 0xc3};
    memcpy(target, mov_eax_42_ret, sizeof(mov_eax_42_ret));

    uintptr_t cell = 0;
    bool took_page_0 = false;
    if (find_cell(next, low, high, cell, took_page_0))
    {
        // Rel32Ind64: jmp [rip+disp32] through the cell, with MonoMod's cast of the displacement
        *reinterpret_cast<uintptr_t *>(cell) = to;
        int32_t disp = static_cast<int32_t>(static_cast<intptr_t>(cell) - static_cast<intptr_t>(next));
        code[0] = 0xff;
        code[1] = 0x25;
        memcpy(code + 2, &disp, sizeof(disp));
        std::printf("far_jump_check: cell at %#llx%s\n", static_cast<unsigned long long>(cell),
                    took_page_0 ? " (MonoMod took page 0, so VirtualAlloc chose the place)" : "");
    }
    else
    {
        // Abs64: the target follows the jump, as MonoMod falls back when no cell is near
        code[0] = 0xff;
        code[1] = 0x25;
        memset(code + 2, 0, 4);
        memcpy(code + 6, &to, sizeof(to));
        std::printf("far_jump_check: no free page within 2 GB of the code; the jump holds its target\n");
    }
    FlushInstructionCache(GetCurrentProcess(), code, 16);

    // A cell out of reach makes the jump read the wrapped address. The call goes on only when that address faults,
    // so the check never jumps into memory that holds something else.
    if (cell && (cell < low || cell >= high))
    {
        uintptr_t wrapped = next + static_cast<intptr_t>(*reinterpret_cast<int32_t *>(code + 2));
        MEMORY_BASIC_INFORMATION info;
        bool readable = VirtualQuery(reinterpret_cast<void *>(wrapped), &info, sizeof(info)) &&
                        info.State == MEM_COMMIT && !(info.Protect & (PAGE_NOACCESS | PAGE_GUARD));
        if (readable)
        {
            char what[160];
            std::snprintf(what, sizeof(what), "the cell is %lld MB from the jump, which would read %#llx",
                          static_cast<long long>((static_cast<intptr_t>(cell) - static_cast<intptr_t>(next)) >> 20),
                          static_cast<unsigned long long>(wrapped));
            return fail(what);
        }
    }

    AddVectoredExceptionHandler(1, on_fault);
    auto call = reinterpret_cast<int (*)()>(code);
    int result = call();
    if (faulted)
    {
        char what[160];
        std::snprintf(what, sizeof(what), "the jump read %#llx and faulted; the cell is %lld MB from the jump",
                      static_cast<unsigned long long>(fault_address),
                      static_cast<long long>((static_cast<intptr_t>(cell) - static_cast<intptr_t>(next)) >> 20));
        return fail(what);
    }
    if (result != 42) return fail("the jump did not reach its target");
    std::printf("far_jump_check: PASS\n");
    return 0;
}
