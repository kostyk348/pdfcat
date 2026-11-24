import struct
p = "shot-real.bmp"
d = open(p, "rb").read()
off = struct.unpack_from("<I", d, 10)[0]
w, h = struct.unpack_from("<ii", d, 18)
bpp = struct.unpack_from("<H", d, 28)[0]
topdown = h < 0
h = abs(h)
print("bmp", w, "x", h, "bpp", bpp, "offset", off, "topdown", topdown)
row = ((w * bpp // 8 + 3) // 4) * 4
cnt = 0
tot = 0
for y in range(60, 860, 4):
    ry = y if topdown else h - 1 - y
    base = off + ry * row
    for x in range(10, 1080, 4):
        i = base + x * (bpp // 8)
        b, g, r = d[i], d[i+1], d[i+2]
        tot += 1
        if b < 200 or g < 200 or r < 200:
            cnt += 1
print("sample_count", tot, "ink_pixels", cnt, "percent", round(100 * cnt / max(1, tot), 2))
