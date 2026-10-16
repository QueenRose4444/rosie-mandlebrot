import mpmath as mp
mp.mp.dps = 150

def nucleus(c0, p, iters=160):
    c = mp.mpc(c0)
    for _ in range(iters):
        z = mp.mpc(0); dz = mp.mpc(0)   # dz = d z_p / d c
        for _k in range(p):
            dz = 2*z*dz + 1
            z = z*z + c
        if dz == 0: break
        step = z/dz
        c -= step
        if abs(step) < mp.mpf(10)**(-135): break
    z = mp.mpc(0)
    for _k in range(p):
        z = z*z + c
    return c, abs(z)

def actual_period(c, pmax, eps=mp.mpf(10)**(-25)):
    z = mp.mpc(0)
    for k in range(1, pmax+1):
        z = z*z + c
        if abs(z) < eps:
            return k
    return None

def size_estimate(c, p):
    z = mp.mpc(0); l = mp.mpc(1); b = mp.mpc(1)
    for _k in range(1, p):
        z = z*z + c
        l = 2*z*l
        if l == 0: return mp.mpf(0)
        b = b + 1/l
    d = b*l*l
    return mp.mpf(0) if d == 0 else abs(1/d)

# Search near the seahorse-valley period-225 nucleus for a deeper (smaller,
# higher-period) mini-brot. Zooming toward it shows seahorse spirals down to
# that mini-brot — the "defined edges all the way" look, but deeper than 1e-30.
guess = mp.mpc('-0.745036916467857316826098125424579854658124181847430152776577',
               '0.113013142775786019881215984800647577862286869909775942351113')
cands = []
for p in range(226, 560):
    c, resid = nucleus(guess, p)
    if resid < mp.mpf(10)**(-120) and abs(c - guess) < mp.mpf('0.01'):
        if actual_period(c, p) == p:
            sz = size_estimate(c, p)
            if sz > 0:
                cands.append((p, c, sz))

cands.sort(key=lambda t: t[2])   # deepest (smallest) first
print("period |   size   | center (short)")
for p, c, sz in cands[:10]:
    print(f"{p:5d} | {mp.nstr(sz,3):>9} | {mp.nstr(c.real,18)}  {mp.nstr(c.imag,18)}")

# Pick the deepest one whose size is still in a comfortably renderable range
# (>= 1e-45, so the zoom + coordinate digits are well within float-exp + rug).
target = None
for p, c, sz in cands:
    if sz >= mp.mpf('1e-45'):
        target = (p, c, sz); break
if target is None and cands:
    target = cands[0]
if target:
    p, c, sz = target
    print("\nCHOSEN  period =", p, " size =", mp.nstr(sz, 5))
    print("re =", mp.nstr(c.real, 130))
    print("im =", mp.nstr(c.imag, 130))
