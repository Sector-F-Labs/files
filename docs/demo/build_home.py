import os, sys, random, math, time
from PIL import Image, ImageDraw, ImageFilter
root = sys.argv[1]
random.seed(7)
def mk(path, size=0, text=None, mtime=None):
    p = os.path.join(root, path); os.makedirs(os.path.dirname(p), exist_ok=True)
    with open(p, "wb") as f:
        if text: f.write(text.encode())
        if size: f.truncate(max(size, len(text or "")))
    if mtime: os.utime(p, (mtime, mtime))
now = time.time(); D = 86400
_u = os.utime
def _ut(p, t):
    t = (t[0] - random.randint(0, 80000), t[1] - random.randint(0, 80000)); _u(p, t)
os.utime = _ut

MB = 1 << 20
# Pictures: generated landscapes, no real photos
def scene(name, sky, sun, hills, seed):
    r = random.Random(seed); W, H = 960, 640
    im = Image.new("RGB", (W, H)); d = ImageDraw.Draw(im)
    for y in range(H):
        t = y / H
        d.line([(0, y), (W, y)], fill=tuple(int(sky[0][i]*(1-t)+sky[1][i]*t) for i in range(3)))
    cx, cy = r.randint(200, 760), r.randint(120, 260)
    d.ellipse([cx-55, cy-55, cx+55, cy+55], fill=sun)
    im = im.filter(ImageFilter.GaussianBlur(2)); d = ImageDraw.Draw(im)
    for k, col in enumerate(hills):
        base = 330 + k*95; ph = r.random()*6; amp = 60 - k*8
        pts = [(0, H)] + [(x, base + amp*math.sin(x/140 + ph) + 25*math.sin(x/53 + ph*2)) for x in range(0, W+1, 8)] + [(W, H)]
        d.polygon(pts, fill=col)
    p = os.path.join(root, "Pictures", name); os.makedirs(os.path.dirname(p), exist_ok=True)
    im.save(p, quality=88)
    t = now - r.randint(3, 200)*D; os.utime(p, (t, t))
sets = {
 "Dawn over the ridge.jpg": (((255,190,140),(120,150,210)), (255,240,200), [(90,100,140),(60,80,110),(35,55,75)]),
 "Harbour, blue hour.jpg": (((40,60,120),(210,150,150)), (250,230,210), [(30,40,70),(20,30,55),(12,20,38)]),
 "Pine valley.jpg": (((150,200,240),(235,245,250)), (255,250,230), [(110,150,120),(70,110,85),(40,75,55)]),
 "Desert road.jpg": (((250,200,120),(250,235,200)), (255,255,235), [(210,150,90),(180,115,70),(140,85,55)]),
 "Fjord morning.jpg": (((170,200,225),(240,235,230)), (255,255,240), [(120,140,170),(80,105,135),(45,70,95)]),
 "Autumn hills.jpg": (((240,170,110),(250,220,170)), (255,245,215), [(190,110,60),(150,80,45),(105,55,35)]),
 "Night ferry.jpg": (((15,20,50),(70,60,110)), (235,235,250), [(25,30,55),(15,20,40),(8,12,28)]),
 "Salt flats.jpg": (((130,185,235),(250,250,250)), (255,255,255), [(200,205,215),(165,175,190),(130,140,160)]),
}
for i, (n, (sky, sun, hills)) in enumerate(sets.items()): scene(n, sky, sun, hills, i+1)
mk("Pictures/Screenshots/Screenshot 2026-10-28 14-02-11.png", 310_000, mtime=now-6*D)
mk("Pictures/Screenshots/Screenshot 2026-10-30 09-41-52.png", 280_000, mtime=now-4*D)
# Documents
docs = {
 "Budget 2026.ods": 84_000, "Invoice 0142.pdf": 213_000, "Invoice 0143.pdf": 198_000,
 "Meeting notes.md": 4_200, "Roadmap Q4.odt": 61_000, "Lease agreement.pdf": 1_240_000,
 "Reading list.txt": 1_800, "Tax summary 2025.pdf": 540_000, "Trip itinerary.md": 3_100,
}
for n, s in docs.items(): mk(f"Documents/{n}", s, text=f"# {n}\n", mtime=now-random.randint(1, 120)*D)
for sub, files in {"Contracts": ["Studio rental.pdf", "Insurance 2026.pdf"], "Receipts": ["Hardware shop.pdf", "Train tickets.pdf", "Software licence.pdf"], "Drafts": ["Essay v1.md", "Essay v2.md", "Talk outline.md"]}.items():
    for f in files: mk(f"Documents/{sub}/{f}", random.randint(3_000, 900_000), text="x", mtime=now-random.randint(5, 300)*D)
# Downloads
for n, s in {"archive-2026-10.tar.zst": 842*MB, "installer.AppImage": 96*MB, "dataset.csv": 18*MB, "wallpaper-pack.zip": 212*MB, "paper.pdf": 3*MB, "release-notes.txt": 6_000, "backup-keys.txt.gpg": 2_300}.items():
    mk(f"Downloads/{n}", s, text="x", mtime=now-random.randint(0, 40)*D)
# Music, Videos
for a, tracks in {"Static Bloom": ["01 Low tide.flac", "02 Paper lanterns.flac", "03 Weather.flac", "04 Nightbus.flac"], "Field Notes": ["01 Morning.mp3", "02 Overpass.mp3", "03 Harbour lights.mp3"]}.items():
    for t in tracks: mk(f"Music/{a}/{t}", random.randint(6, 38)*MB, text="x", mtime=now-random.randint(30, 400)*D)
for n, s in {"Conference talk.mkv": 1400*MB, "Hike, September.mp4": 2100*MB, "Screen recording.mp4": 310*MB}.items(): mk(f"Videos/{n}", s, text="x", mtime=now-random.randint(2, 90)*D)
# Projects: a small source tree
for f, s in {"README.md": 2400, "Cargo.toml": 640, "Cargo.lock": 38_000, "LICENSE": 1_500, ".gitignore": 40, "Makefile": 900, "src/main.rs": 5_600, "src/lib.rs": 3_100, "src/config.rs": 4_800, "src/render.rs": 12_300, "docs/design.md": 7_700, "tests/smoke.rs": 2_200}.items():
    mk(f"Projects/lantern/{f}", s, text="x", mtime=now-random.randint(0, 20)*D)
for f in ["main.py", "pyproject.toml", "notes.md"]: mk(f"Projects/tidepool/{f}", random.randint(800, 9000), text="x", mtime=now-random.randint(10, 60)*D)
mk("Projects/sketches/ideas.txt", 900, text="x", mtime=now-30*D)
# Apps: Name.app folders
def app(name, ident, summary, color, shape):
    base = f"Apps/{name}.app"
    mk(f"{base}/app.toml", text=f'format = 1\nname = "{name}"\nexec = "bin/{name.lower()}"\nid = "com.example.{ident}"\nsummary = "{summary}"\nicon = "icon.svg"\n')
    mk(f"{base}/bin/{name.lower()}", 1000, text="#!/bin/sh\n")
    os.chmod(os.path.join(root, f"{base}/bin/{name.lower()}"), 0o755)
    shapes = {
     "circle": '<circle cx="64" cy="64" r="26" fill="#fff" fill-opacity=".92"/>',
     "bars": '<rect x="34" y="70" width="14" height="24" rx="3" fill="#fff"/><rect x="57" y="52" width="14" height="42" rx="3" fill="#fff"/><rect x="80" y="36" width="14" height="58" rx="3" fill="#fff"/>',
     "page": '<rect x="38" y="30" width="52" height="68" rx="6" fill="#fff"/><rect x="48" y="46" width="32" height="5" rx="2" fill="%s"/><rect x="48" y="60" width="32" height="5" rx="2" fill="%s"/><rect x="48" y="74" width="20" height="5" rx="2" fill="%s"/>' % (color, color, color),
     "play": '<polygon points="52,38 52,90 94,64" fill="#fff"/>',
     "note": '<circle cx="52" cy="88" r="12" fill="#fff"/><rect x="60" y="34" width="8" height="54" fill="#fff"/><path d="M68 34 q22 4 22 24 q-12 -10 -22 -8z" fill="#fff"/>',
     "wave": '<path d="M26 64 q12 -30 24 0 t24 0 t24 0" stroke="#fff" stroke-width="9" fill="none" stroke-linecap="round"/>',
     "grid": '<rect x="34" y="34" width="26" height="26" rx="5" fill="#fff"/><rect x="68" y="34" width="26" height="26" rx="5" fill="#fff" fill-opacity=".7"/><rect x="34" y="68" width="26" height="26" rx="5" fill="#fff" fill-opacity=".7"/><rect x="68" y="68" width="26" height="26" rx="5" fill="#fff"/>',
    }
    mk(f"{base}/icon.svg", text=f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128"><defs><linearGradient id="g" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{color}"/><stop offset="1" stop-color="{color}" stop-opacity=".72"/></linearGradient></defs><rect x="8" y="8" width="112" height="112" rx="26" fill="url(#g)"/>{shapes[shape]}</svg>')
for a in [("Notebook","notebook","Notes and documents","#3d8fe0","page"),("Reader","reader","Read PDFs and books","#d9822b","page"),("Montage","montage","Edit video","#8a5cd0","play"),("UnAmp","unamp","Music player","#d94f5c","note"),("Graphs","graphs","Charts from tables","#2f9e72","bars"),("Radio","radio","Internet radio","#c2418f","wave"),("Dashboard","dashboard","System overview","#4a6fa5","grid"),("Beacon","beacon","Status light","#e0a526","circle")]: app(*a)
# Desktop, and hidden-ish extras in home
mk("Desktop/Todo.txt", 400, text="x", mtime=now-D)
mk("notes-scratch.txt", 700, text="x", mtime=now-2*D)

os.makedirs(os.path.join(root, ".config"), exist_ok=True)
open(os.path.join(root, ".config/user-dirs.dirs"), "w").write("\n".join(f'XDG_{k}_DIR="$HOME/{v}"' for k, v in [("DESKTOP","Desktop"),("DOCUMENTS","Documents"),("DOWNLOAD","Downloads"),("MUSIC","Music"),("PICTURES","Pictures"),("VIDEOS","Videos")]) + "\n")
