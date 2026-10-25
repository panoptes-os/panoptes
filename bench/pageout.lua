-- Page out each PID's private anonymous memory with process_madvise(2) and
-- MADV_PAGEOUT, whichever cgroup holds the charge. The kernel skips pages
-- that another process also maps, such as a zygote's copy-on-write pages.
-- No packaged tool makes this call. Run as root under LuaJIT, whose FFI can
-- make raw syscalls. Prints "PID RANGE_MIB ERRORS" per PID: the size of
-- the ranges advised, mostly reserved and unpopulated, not what was freed.
-- usage: luajit pageout.lua PID...
local ffi = require("ffi")
ffi.cdef [[
struct iovec { void *iov_base; size_t iov_len; };
long syscall(long number, ...);
int close(int fd);
char *strerror(int errnum);
]]
local C = ffi.C
-- x86_64 syscall numbers, and MADV_PAGEOUT from uapi/asm-generic/mman-common.h
local SYS_pidfd_open, SYS_process_madvise, MADV_PAGEOUT = 434, 440, 21
local function long(v) return ffi.cast("long", v) end

for _, pid in ipairs(arg) do
  local fd = tonumber(C.syscall(SYS_pidfd_open, long(tonumber(pid)), long(0)))
  if fd < 0 then
    io.stderr:write(("pageout: pidfd_open %s: %s\n"):format(
      pid, ffi.string(C.strerror(ffi.errno()))))
  else
    local total, errors = 0, 0
    local iov = ffi.new("struct iovec[1]")
    -- One call per range, so a range the kernel refuses (locked or special
    -- mappings) does not stop the ranges after it.
    for line in io.lines("/proc/" .. pid .. "/maps") do
      local lo, hi, perms, path =
        line:match("^(%x+)%-(%x+) (%S+) %S+ %S+ %S+%s*(.*)$")
      local private_rw = perms:sub(2, 2) == "w" and perms:sub(4, 4) == "p"
      if private_rw and (path == "" or path:sub(1, 1) == "[") then
        local a, b = tonumber(lo, 16), tonumber(hi, 16)
        iov[0].iov_base = ffi.cast("void *", ffi.cast("uintptr_t", a))
        iov[0].iov_len = b - a
        local got = tonumber(C.syscall(SYS_process_madvise, long(fd), iov,
          long(1), long(MADV_PAGEOUT), long(0)))
        if got < 0 then errors = errors + 1 else total = total + got end
      end
    end
    C.close(fd)
    print(pid, math.floor(total / 1048576), errors)
  end
end
