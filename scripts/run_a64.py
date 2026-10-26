#!/usr/bin/env python3
"""Run a static AArch64 executable produced by `flash build --target aarch64-unknown-linux-gnu`
(ELF) or `--target aarch64-apple-darwin` (Mach-O) in the Unicorn CPU emulator. The few system
calls the Flash runtime uses are emulated here (Linux numbering, or the macOS BSD one with its
carry-flag error convention).

usage: run_a64.py prog [args...]   (stdin/stdout are passed through)
"""
import os, struct, sys, time
from unicorn import *
from unicorn.arm64_const import *

def main():
    path = sys.argv[1]
    data = open(path, 'rb').read()
    mu = Uc(UC_ARCH_ARM64, UC_MODE_ARM)
    # enable FP/SIMD
    mu.reg_write(UC_ARM64_REG_CPACR_EL1, 0x300000)
    macos = data[:4] == b'\xcf\xfa\xed\xfe'
    if macos:
        # tpidrro_el0 points at the thread-specific-data array of the (only) thread
        TSD = 0x500000000
        mu.mem_map(TSD, 0x4000)
        mu.reg_write(UC_ARM64_REG_TPIDRRO_EL0, TSD)
    segs = []  # (vaddr, memsz, fileoff, filesz)
    if macos:
        assert struct.unpack_from('<I', data, 4)[0] == 0x0100000C, "not an arm64 Mach-O"
        ncmds = struct.unpack_from('<I', data, 16)[0]
        off = 32
        entry = None
        main_off = None
        data_vmaddr = None
        text_base = None
        for _ in range(ncmds):
            cmd, size = struct.unpack_from('<II', data, off)
            if cmd == 0x19:
                name = data[off + 8:off + 24].rstrip(b'\0')
                vmaddr, vmsize, fileoff, filesize = struct.unpack_from('<QQQQ', data, off + 24)
                if name == b'__TEXT':
                    text_base = vmaddr
                if name == b'__DATA':
                    data_vmaddr = vmaddr
                if name != b'__PAGEZERO':
                    segs.append((vmaddr, vmsize, fileoff, filesize))
            elif cmd == 0x80000028:  # LC_MAIN: entry offset from the start of __TEXT
                main_off = struct.unpack_from('<Q', data, off + 8)[0]
            off += size
        assert main_off is not None, 'no LC_MAIN'
        entry = text_base + main_off
    else:
        assert data[:4] == b'\x7fELF' and struct.unpack_from('<H', data, 18)[0] == 183, "not an aarch64 ELF"
        entry = struct.unpack_from('<Q', data, 24)[0]
        phoff = struct.unpack_from('<Q', data, 32)[0]
        phentsize, phnum = struct.unpack_from('<HH', data, 54)
        for i in range(phnum):
            t, flags, off, vaddr, paddr, filesz, memsz, align = struct.unpack_from('<IIQQQQQQ', data, phoff + i * phentsize)
            if t == 1:
                segs.append((vaddr, memsz, off, filesz))
    for vaddr, memsz, off, filesz in segs:
        lo = vaddr & ~0xFFF
        hi = (vaddr + memsz + 0xFFF) & ~0xFFF
        try:
            mu.mem_map(lo, hi - lo)
        except UcError:
            pass
        mu.mem_write(vaddr, data[off:off + filesz])
    STACK = 0x7ff00000
    SSIZE = 0x100000
    mu.mem_map(STACK - SSIZE, SSIZE + 0x10000)
    args = [os.path.abspath(path).encode()] + [a.encode() for a in sys.argv[2:]]
    sp = STACK
    ptrs = []
    for a in reversed(args):
        sp -= len(a) + 1
        mu.mem_write(sp, a + b'\0')
        ptrs.append(sp)
    ptrs.reverse()
    sp &= ~15
    blob = struct.pack('<Q', len(args)) + b''.join(struct.pack('<Q', p) for p in ptrs) + struct.pack('<QQ', 0, 0)
    sp -= (len(blob) + 15) & ~15
    mu.mem_write(sp, blob)
    mu.reg_write(UC_ARM64_REG_SP, sp)
    brk = [0x10000000]
    mmap_next = [0x20000000]
    state = {'exit': None}
    fds = {}


    # ---- cooperative threads (macOS: pthread_create & co. imported through the bind table at the start of __DATA)
    REGS = [getattr(__import__('unicorn.arm64_const', fromlist=['x']), 'UC_ARM64_REG_X%d' % i) for i in range(31)]
    ctxs = []          # runnable thread contexts, the running one first
    finished = set()
    nthreads = [1]
    cur = {'id': 0}
    region = [0x700000000]
    def alloc_region(n):
        a = region[0]; mu.mem_map(a, n); region[0] += n + 0x10000; return a
    def save_ctx():
        c = {'x': [mu.reg_read(r) for r in REGS], 'sp': mu.reg_read(UC_ARM64_REG_SP), 'pc': mu.reg_read(UC_ARM64_REG_PC),
             'nzcv': mu.reg_read(UC_ARM64_REG_NZCV), 'tp': mu.reg_read(UC_ARM64_REG_TPIDRRO_EL0), 'id': cur['id']}
        return c
    def load_ctx(c):
        for r, v in zip(REGS, c['x']): mu.reg_write(r, v)
        mu.reg_write(UC_ARM64_REG_SP, c['sp']); mu.reg_write(UC_ARM64_REG_NZCV, c['nzcv'])
        mu.reg_write(UC_ARM64_REG_TPIDRRO_EL0, c['tp']); mu.reg_write(UC_ARM64_REG_PC, c['pc']); cur['id'] = c['id']
    def switch(resume_pc=None):
        """Suspend the running thread (it resumes at resume_pc, default: where it is) and run the next one."""
        if not ctxs: return
        me = save_ctx()
        if resume_pc is not None: me['pc'] = resume_pc
        nxt = ctxs.pop(0); ctxs.append(me); load_ctx(nxt)
    def thread_exit():
        finished.add(cur['id'])
        if not ctxs:
            state['exit'] = 0; mu.emu_stop(); return
        load_ctx(ctxs.pop(0))
    FAKE = 0x6000000000
    NAMES = ['pthread_create', 'pthread_join', 'pthread_attr_init', 'pthread_attr_setstacksize']
    EXIT_ADDR = FAKE + 0x800
    def bind_hook(mu_, addr, size, ud):
        lr = mu.reg_read(UC_ARM64_REG_X30)
        if addr == EXIT_ADDR:
            thread_exit(); return
        name = NAMES[(addr - FAKE) // 16]
        if name in ('pthread_attr_init', 'pthread_attr_setstacksize'):
            mu.reg_write(UC_ARM64_REG_X0, 0); mu.reg_write(UC_ARM64_REG_PC, lr)
        elif name == 'pthread_create':
            tid_ptr, _attr, fn, arg = [mu.reg_read(r) for r in (UC_ARM64_REG_X0, UC_ARM64_REG_X1, UC_ARM64_REG_X2, UC_ARM64_REG_X3)]
            tid = nthreads[0]; nthreads[0] += 1
            tsd = alloc_region(0x4000); stack = alloc_region(0x400000)
            xs = [0] * 31; xs[0] = arg; xs[30] = EXIT_ADDR
            ctxs.append({'x': xs, 'sp': stack + 0x400000 - 64, 'pc': fn, 'nzcv': 0, 'tp': tsd, 'id': tid})
            mu.mem_write(tid_ptr, struct.pack('<Q', tid))
            mu.reg_write(UC_ARM64_REG_X0, 0); mu.reg_write(UC_ARM64_REG_PC, lr)
        elif name == 'pthread_join':
            tid = mu.reg_read(UC_ARM64_REG_X0)
            if tid in finished:
                mu.reg_write(UC_ARM64_REG_X0, 0); mu.reg_write(UC_ARM64_REG_PC, lr)
            else:
                switch(resume_pc=addr)   # try again when this thread runs next
    if macos and data_vmaddr is not None:
        mu.mem_map(FAKE, 0x1000)
        i = 0
        while True:
            w, = struct.unpack('<Q', bytes(mu.mem_read(data_vmaddr + 8 * i, 8)))
            if not (w >> 63): break
            mu.mem_write(data_vmaddr + 8 * i, struct.pack('<Q', FAKE + 16 * (w & 0xFFFFFF)))
            i += 1
            if ((w >> 51) & 0xFFF) == 0: break
        mu.hook_add(UC_HOOK_CODE, bind_hook, None, FAKE, FAKE + 0x900)

    def cstr(addr):
        out = b''
        while True:
            c = mu.mem_read(addr, 1)
            if c == b'\0':
                return out.decode()
            out += bytes(c); addr += 1

    def sysc(mu, intno, user):
        n = mu.reg_read(UC_ARM64_REG_X8)
        if macos:
            # BSD number in x16 -> the Linux number of the same call
            bsd = mu.reg_read(UC_ARM64_REG_X16)
            n = {1: 94, 3: 63, 4: 64, 5: -56, 6: 57, 10: -35, 33: -48, 73: 215, 197: 222, 116: -113, 93: -101, 515: 98, 516: 98, 20: 172}.get(bsd, 9999)
        a = [mu.reg_read(r) for r in (UC_ARM64_REG_X0, UC_ARM64_REG_X1, UC_ARM64_REG_X2, UC_ARM64_REG_X3, UC_ARM64_REG_X4, UC_ARM64_REG_X5)]
        def sg(x): return x - (1 << 64) if x >> 63 else x
        r = -38
        if n < 0:  # macOS-style call: translate the arguments to the Linux form
            n = -n
            if n == 56:   a = [0, a[0], {0: 0, 1537: 577, 521: 1089}.get(a[1], a[1]), a[2], 0, 0]
            elif n == 35: a = [0, a[0], 0, 0, 0, 0]
            elif n == 48: a = [0, a[0], 0, 0, 0, 0]
            elif n == 113:
                t = time.time_ns()
                mu.mem_write(a[0], struct.pack('<qq', t // 10**9, (t % 10**9) // 1000)); n = 0; r = 0
            elif n == 101:
                sec, us = struct.unpack('<qq', bytes(mu.mem_read(a[4], 16))); time.sleep(sec + us / 1e6); n = 0; r = 0
        if n == 0:
            pass
        elif n == 64:  # write
            b = bytes(mu.mem_read(a[1], a[2]))
            if a[0] == 1: sys.stdout.buffer.write(b); sys.stdout.buffer.flush()
            elif a[0] == 2: sys.stderr.buffer.write(b); sys.stderr.buffer.flush()
            else: os.write(fds[a[0]], b)
            r = len(b)
        elif n == 63:  # read
            try:
                b = os.read(0 if a[0] == 0 else fds[a[0]], a[2])
                mu.mem_write(a[1], b); r = len(b)
            except OSError as e: r = -e.errno
        elif n == 56:  # openat
            fl = a[2]
            try:
                fd = os.open(cstr(a[1]), fl, a[3]); fds[fd + 100] = fd; r = fd + 100
            except OSError as e: r = -e.errno
        elif n == 57:
            if a[0] in fds: os.close(fds.pop(a[0]))
            r = 0
        elif n == 222:  # mmap
            ln = (a[1] + 0xFFF) & ~0xFFF
            addr = mmap_next[0]; mmap_next[0] += ln + 0x1000
            mu.mem_map(addr, ln); r = addr
        elif n == 172:  # getpid
            r = 1
        elif n in (215, 98, 124, 226):  # munmap, futex (nothing ever waits: one thread), sched_yield, mprotect
            r = 0
        elif n in (93, 94):
            state['exit'] = sg(a[0]) & 0xFF; mu.emu_stop(); return
        elif n == 113:
            t = time.monotonic_ns()
            mu.mem_write(a[1], struct.pack('<qq', t // 10**9, t % 10**9)); r = 0
        elif n == 101:
            sec, ns = struct.unpack('<qq', bytes(mu.mem_read(a[0], 16))); time.sleep(sec + ns / 1e9); r = 0
        elif n == 48:
            r = 0 if os.path.exists(cstr(a[1])) else -2
        elif n == 35:
            try: os.unlink(cstr(a[1])); r = 0
            except OSError as e: r = -e.errno
        else:
            sys.stderr.write("unsupported syscall %d\n" % n); state['exit'] = 111; mu.emu_stop(); return
        if macos:
            nzcv = mu.reg_read(UC_ARM64_REG_NZCV)
            if -4096 < r < 0:
                mu.reg_write(UC_ARM64_REG_NZCV, nzcv | (1 << 29)); r = -r
            else:
                mu.reg_write(UC_ARM64_REG_NZCV, nzcv & ~(1 << 29))
        mu.reg_write(UC_ARM64_REG_X0, r & ((1 << 64) - 1))
        if macos and ctxs and bsd in (515, 93):
            switch()   # a thread waiting for something lets the others run

    mu.hook_add(UC_HOOK_INTR, sysc)
    if macos:
        # like dyld: call main(argc, argv, envp) with the original stack
        mu.reg_write(UC_ARM64_REG_X0, len(args))
        mu.reg_write(UC_ARM64_REG_X1, sp + 8)
        mu.reg_write(UC_ARM64_REG_X2, sp + 8 + 8 * (len(args) + 1))
    try:
        mu.emu_start(entry, 0)
    except UcError as e:
        pc = mu.reg_read(UC_ARM64_REG_PC)
        sys.stderr.write("emulation error: %s at pc=%#x\n" % (e, pc))
        sys.exit(139)
    if os.environ.get('A64_DEBUG'):
        sys.stderr.write('stopped at pc=%#x lr=%#x sp=%#x thread=%d\n' % (mu.reg_read(UC_ARM64_REG_PC), mu.reg_read(UC_ARM64_REG_X30), mu.reg_read(UC_ARM64_REG_SP), cur['id']))
    sys.exit(state['exit'] if state['exit'] is not None else 0)

main()
