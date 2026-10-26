#!/usr/bin/env python3
"""Run a Windows x86-64 PE executable produced by `flash build --target x86_64-pc-windows-msvc`
in the Unicorn CPU emulator. The kernel32 functions the Flash runtime imports are emulated here.

usage: run_win.py prog.exe [args...]   (stdin/stdout are passed through)
"""
import os, struct, sys, time
from unicorn import *
from unicorn.x86_const import *

def main():
    path = sys.argv[1]
    d = open(path, 'rb').read()
    assert d[:2] == b'MZ'
    pe = struct.unpack_from('<I', d, 0x3C)[0]
    assert d[pe:pe + 4] == b'PE\0\0'
    nsec, = struct.unpack_from('<H', d, pe + 6)
    optsz, = struct.unpack_from('<H', d, pe + 20)
    opt = pe + 24
    assert struct.unpack_from('<H', d, opt)[0] == 0x20B
    entry_rva, = struct.unpack_from('<I', d, opt + 16)
    base, = struct.unpack_from('<Q', d, opt + 24)
    size_of_image, = struct.unpack_from('<I', d, opt + 56)
    ddir = opt + 112
    imp_rva, imp_size = struct.unpack_from('<II', d, ddir + 8)
    mu = Uc(UC_ARCH_X86, UC_MODE_64)
    mu.mem_map(base, (size_of_image + 0xFFF) & ~0xFFF)
    sec = opt + optsz
    for i in range(nsec):
        name, vsize, rva, rawsize, rawoff = struct.unpack_from('<8sIIII', d, sec + 40 * i)
        mu.mem_write(base + rva, d[rawoff:rawoff + min(rawsize, vsize if vsize else rawsize)] if rawsize else b'')
    # resolve imports: put a fake address in every IAT slot
    FAKE = 0x7F0000000
    mu.mem_map(FAKE, 0x1000)
    names = {}
    off = imp_rva
    while True:
        oft, ts, fwd, nameoff, ft = struct.unpack('<IIIII', bytes(mu.mem_read(base + off, 20)))
        if nameoff == 0:
            break
        k = 0
        while True:
            thunk, = struct.unpack('<Q', bytes(mu.mem_read(base + oft + 8 * k, 8)))
            if thunk == 0:
                break
            hn = bytes(mu.mem_read(base + (thunk & 0x7FFFFFFF) + 2, 64)).split(b'\0')[0].decode()
            addr = FAKE + 16 * len(names)
            names[addr] = hn
            mu.mem_write(base + ft + 8 * k, struct.pack('<Q', addr))
            k += 1
        off += 20
    # process state
    STACK = 0x7FF000000
    mu.mem_map(STACK - 0x800000, 0x800000 + 0x1000)
    heap = [0x200000000]
    cmdline = ' '.join(['"' + os.path.abspath(path) + '"'] + [('"%s"' % a if ' ' in a else a) for a in sys.argv[2:]]).encode() + b'\0'
    envblock = b''.join(('%s=%s' % (k, v)).encode() + b'\0' for k, v in os.environ.items()) + b'\0'
    def alloc(n):
        a = heap[0]; ln = (n + 0xFFF) & ~0xFFF
        mu.mem_map(a, ln); heap[0] += ln + 0x1000
        return a
    cmd_addr = alloc(len(cmdline)); mu.mem_write(cmd_addr, cmdline)
    env_addr = alloc(len(envblock)); mu.mem_write(env_addr, envblock)
    handles = {0x10: 0, 0x20: 1, 0x30: 2}
    nexth = [0x100]
    last_error = [0]
    state = {'exit': None}
    R = lambda r: mu.reg_read(r)
    def cstr(a):
        out = b''
        while True:
            c = bytes(mu.mem_read(a, 1))
            if c == b'\0': return out.decode('latin-1')
            out += c; a += 1
    def api(name):
        rcx, rdx, r8, r9 = R(UC_X86_REG_RCX), R(UC_X86_REG_RDX), R(UC_X86_REG_R8), R(UC_X86_REG_R9)
        rsp = R(UC_X86_REG_RSP)
        assert rsp % 16 == 8, "stack misaligned at call to %s (rsp=%#x)" % (name, rsp)
        stk = lambda i: struct.unpack('<Q', bytes(mu.mem_read(rsp + 0x28 + 8 * i, 8)))[0]
        r = 0
        if name == 'GetStdHandle':
            r = {0xFFFFFFF6: 0x10, 0xFFFFFFF5: 0x20, 0xFFFFFFF4: 0x30}.get(rcx & 0xFFFFFFFF, 0xFFFFFFFFFFFFFFFF)
        elif name == 'WriteFile':
            b = bytes(mu.mem_read(rdx, r8 & 0xFFFFFFFF)); fd = handles[rcx]
            if fd == 1: sys.stdout.buffer.write(b); sys.stdout.buffer.flush()
            elif fd == 2: sys.stderr.buffer.write(b); sys.stderr.buffer.flush()
            else: os.write(fd, b)
            mu.mem_write(r9, struct.pack('<I', len(b))); r = 1
        elif name == 'ReadFile':
            try:
                b = os.read(handles[rcx], r8 & 0xFFFFFFFF); mu.mem_write(rdx, b); mu.mem_write(r9, struct.pack('<I', len(b))); r = 1
            except OSError: last_error[0] = 5; r = 0
        elif name == 'CreateFileA':
            access, disp = rdx & 0xFFFFFFFF, stk(1) & 0xFFFFFFFF
            p = cstr(rcx)
            try:
                if access == 0x80000000: fl = os.O_RDONLY
                elif access == 0x40000000: fl = os.O_WRONLY | os.O_CREAT | os.O_TRUNC
                else: fl = os.O_WRONLY | os.O_CREAT | os.O_APPEND
                fd = os.open(p, fl, 0o666); h = nexth[0]; nexth[0] += 4; handles[h] = fd; r = h
            except FileNotFoundError: last_error[0] = 2; r = 0xFFFFFFFFFFFFFFFF
            except OSError: last_error[0] = 5; r = 0xFFFFFFFFFFFFFFFF
        elif name == 'CloseHandle':
            fd = handles.pop(rcx, None)
            if fd is not None and fd > 2: os.close(fd)
            r = 1
        elif name == 'VirtualAlloc':
            r = alloc(rdx)
        elif name == 'VirtualFree':
            r = 1
        elif name == 'ExitProcess':
            state['exit'] = rcx & 0xFFFFFFFF; mu.emu_stop(); return
        elif name == 'QueryPerformanceCounter':
            mu.mem_write(rcx, struct.pack('<q', time.perf_counter_ns() // 100)); r = 1
        elif name == 'QueryPerformanceFrequency':
            mu.mem_write(rcx, struct.pack('<q', 10_000_000)); r = 1
        elif name == 'Sleep':
            time.sleep((rcx & 0xFFFFFFFF) / 1000.0)
        elif name == 'GetFileAttributesA':
            r = 0x80 if os.path.exists(cstr(rcx)) else 0xFFFFFFFF
        elif name == 'DeleteFileA':
            try: os.unlink(cstr(rcx)); r = 1
            except OSError: r = 0
        elif name == 'GetCommandLineA':
            r = cmd_addr
        elif name == 'GetEnvironmentStringsA':
            r = env_addr
        elif name == 'GetLastError':
            r = last_error[0]
        elif name == 'SwitchToThread':
            r = 0
        elif name == 'CreateThread':
            r = 0  # threads cannot be emulated; the runtime reports "could not start a thread"
        elif name == 'WaitForSingleObject':
            r = 0
        else:
            raise RuntimeError('unemulated import ' + name)
        mu.reg_write(UC_X86_REG_RAX, r)
        ret, = struct.unpack('<Q', bytes(mu.mem_read(rsp, 8)))
        mu.reg_write(UC_X86_REG_RSP, rsp + 8)
        mu.reg_write(UC_X86_REG_RIP, ret)
    def hook(mu_, addr, size, ud):
        api(names[addr])
    mu.hook_add(UC_HOOK_CODE, hook, None, FAKE, FAKE + 16 * len(names))
    # the TEB (gs:[0x28] is the user pointer the runtime keeps its thread state in)
    TEB = 0x7F8000000
    mu.mem_map(TEB, 0x1000)
    mu.reg_write(UC_X86_REG_GS_BASE, TEB)
    # the loader calls the entry point like a function: rsp = 8 mod 16
    HALT = STACK - 0x100
    mu.mem_write(HALT, b'\xF4')
    sp = STACK - 0x1000
    sp -= 8
    mu.mem_write(sp, struct.pack('<Q', HALT))
    mu.reg_write(UC_X86_REG_RSP, sp)
    try:
        mu.emu_start(base + entry_rva, HALT)
    except UcError as e:
        sys.stderr.write("emulation error: %s at rip=%#x\n" % (e, R(UC_X86_REG_RIP)))
        sys.exit(139)
    sys.exit(state['exit'] if state['exit'] is not None else 0)

main()
