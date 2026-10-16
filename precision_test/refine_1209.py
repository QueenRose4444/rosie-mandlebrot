import mpmath as mp
mp.mp.dps = 160
P = 1209
guess = mp.mpc('-0.7419667367318059', '0.1097497907960199')
c = mp.mpc(guess)
for _ in range(200):
    z = mp.mpc(0); dz = mp.mpc(0)
    for _k in range(P):
        dz = 2*z*dz + 1
        z = z*z + c
    if dz == 0: break
    step = z/dz
    c -= step
    if abs(step) < mp.mpf(10)**(-150): break
# residual + true period + size
z = mp.mpc(0)
for _k in range(P):
    z = z*z + c
resid = abs(z)
def aper(c, pmax, eps=mp.mpf(10)**(-25)):
    z=mp.mpc(0)
    for k in range(1,pmax+1):
        z=z*z+c
        if abs(z)<eps: return k
    return None
def size(c,p):
    z=mp.mpc(0); l=mp.mpc(1); b=mp.mpc(1)
    for _k in range(1,p):
        z=z*z+c; l=2*z*l
        b=b+1/l
    return abs(1/(b*l*l))
print("residual", mp.nstr(resid,3), "true_period", aper(c,P), "size", mp.nstr(size(c,P),5))
print("re =", mp.nstr(c.real,140))
print("im =", mp.nstr(c.imag,140))
