import mpmath as mp
mp.mp.dps = 90

def newton_nucleus(c0, p, iters=120):
    c = mp.mpc(c0)
    for _ in range(iters):
        z = mp.mpc(0); dz = mp.mpc(0)  # dz = d z_p / d c
        for _k in range(p):
            dz = 2*z*dz + 1
            z = z*z + c
        if dz == 0: break
        step = z/dz
        c = c - step
        if abs(step) < mp.mpf(10)**(-80): break
    # residual = |z_p(0,c)| (0 at a true period-p nucleus)
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
    denom = b*l*l
    return mp.mpf(0) if denom == 0 else abs(1/denom)

guess = mp.mpc('-0.745037152222279201120686009431143762042', '0.1130130928047116971184681105042953466067')
cands = []
for p in range(116, 320):
    c, resid = newton_nucleus(guess, p)
    if resid < mp.mpf(10)**(-55) and abs(c-guess) < mp.mpf('0.03'):
        ap = actual_period(c, p)
        if ap == p:
            sz = size_estimate(c, p)
            cands.append((p, c, sz))

cands.sort(key=lambda t: t[2])  # smallest (deepest) first
print("period |   size est   | center")
for p, c, sz in cands:
    print(f"{p:6d} | {mp.nstr(sz,3):>12} | {mp.nstr(c.real,40)}  {mp.nstr(c.imag,40)}")

# pick a target: smallest size that is still >= 1e-23 (inside DS range, but deep)
target = None
for p, c, sz in cands:  # ascending by size; take smallest that DS can still resolve
    if sz >= mp.mpf('1e-23'):
        target = (p, c, sz); break
if target is None and cands:
    target = cands[0]
if target:
    p, c, sz = target
    print("\nCHOSEN TARGET")
    print("period =", p)
    print("size   =", mp.nstr(sz, 5))
    print("re =", mp.nstr(c.real, 60))
    print("im =", mp.nstr(c.imag, 60))
