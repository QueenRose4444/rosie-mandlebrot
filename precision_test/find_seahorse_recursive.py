import mpmath as mp
mp.mp.dps = 160

def nucleus(c0, p, iters=140):
    c = mp.mpc(c0)
    for _ in range(iters):
        z = mp.mpc(0); dz = mp.mpc(0)
        for _k in range(p):
            dz = 2*z*dz + 1
            z = z*z + c
        if dz == 0: break
        step = z/dz
        c -= step
        if abs(step) < mp.mpf(10)**(-145): break
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

def deepest_near(guess, plo, phi):
    best = None
    for p in range(plo, phi):
        c, resid = nucleus(guess, p)
        if resid < mp.mpf(10)**(-140) and abs(c - guess) < mp.mpf('0.02'):
            if actual_period(c, p) == p:
                sz = size_estimate(c, p)
                if sz > 0 and (best is None or sz < best[2]):
                    best = (p, c, sz)
    return best

# Start from the period-263 seahorse-valley nucleus (size ~1.2e-21)
# and recurse: recenter on each find and search a higher period window for a
# deeper satellite. Stop once we pass ~1e-38 (deeper than seahorse_deep's 1e-30).
P = 263
guess = mp.mpc('-0.7436134110125930468404113373286616809689078186943304146913010762449747602741667635491935860937210304998397124476071005912297811391',
               '0.1098111333090087714279551097949123141197272432956742113771933511441416105227254197346473713991797334140092652686169897317304312419')
sz = mp.mpf('1.17e-21')
best = (P, guess, sz)
for level in range(6):
    nb = deepest_near(guess, P + 1, P + 230)
    if nb is None:
        print(f"level {level}: no deeper nucleus in ({P+1},{P+230})")
        break
    P, guess, sz = nb
    best = nb
    print(f"level {level}: period {P}  size {mp.nstr(sz,4)}  re {mp.nstr(guess.real,16)} im {mp.nstr(guess.imag,16)}")
    if sz < mp.mpf('1e-38'):
        break

P, guess, sz = best
print("\nFINAL period", P, "size", mp.nstr(sz, 5))
print("re =", mp.nstr(guess.real, 140))
print("im =", mp.nstr(guess.imag, 140))
