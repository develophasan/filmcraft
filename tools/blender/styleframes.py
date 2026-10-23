"""Kadim Atlas style frames: map backgrounds (no text) in three bases, plus where each place lands
on screen (JSON) so the typography and graphics can be laid out afterwards.

    blender -b --factory-startup --python styleframes.py -- job.json

job.json: {"base": "uydu|bakir|karma", "frame": "acilis|rota|sayim", "out": "x.png",
           "tiles": "<folder of terrarium z6 tiles named z_x_y.png>", "bm": "<Blue Marble crop jpg>",
           "bbox": [lon0, lat0, lon1, lat1], "places": {"Efes": [lat, lon], ...}, "width": 1920, "height": 1080}

Reuses crstudio_shot.py (same folder) for the terrain, coast and lights.
"""

import json
import math
import os
import sys

import bpy
import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
ARGS = sys.argv[sys.argv.index("--") + 1 :]
JOB = json.load(open(ARGS[0], encoding="utf-8"))
BASE = JOB.get("base", "uydu")
FRAME = JOB.get("frame", "acilis")
LON0, LAT0, LON1, LAT1 = JOB["bbox"]

# --------------------------------------------------------------- heights from terrarium tiles
Z = 6
N = 1 << Z


def gpx(lon, lat):
    x = (lon + 180.0) / 360.0 * N * 256
    lat_r = math.radians(lat)
    y = (1 - math.log(math.tan(lat_r) + 1 / math.cos(lat_r)) / math.pi) / 2 * N * 256
    return x, y


def tile_pixels(path):
    img = bpy.data.images.load(path, check_existing=False)
    img.colorspace_settings.name = "Non-Color"
    w, h = img.size
    a = np.array(img.pixels[:], dtype=np.float32).reshape(h, w, 4)[::-1]  # top row first
    bpy.data.images.remove(img)
    rgb = np.round(a[..., :3] * 255.0)
    return rgb[..., 0] * 256 + rgb[..., 1] + rgb[..., 2] / 256 - 32768


def heights(gw, gh):
    x0, y0 = gpx(LON0, LAT1)
    x1, y1 = gpx(LON1, LAT0)
    tx0, tx1, ty0, ty1 = int(x0 // 256), int(x1 // 256), int(y0 // 256), int(y1 // 256)
    mos = np.zeros(((ty1 - ty0 + 1) * 256, (tx1 - tx0 + 1) * 256), np.float32)
    for ty in range(ty0, ty1 + 1):
        for tx in range(tx0, tx1 + 1):
            p = os.path.join(JOB["tiles"], f"{Z}_{tx}_{ty}.png")
            if os.path.isfile(p):
                mos[(ty - ty0) * 256 : (ty - ty0 + 1) * 256, (tx - tx0) * 256 : (tx - tx0 + 1) * 256] = tile_pixels(p)
    lons = LON0 + (LON1 - LON0) * (np.arange(gw) + 0.5) / gw
    lats = LAT1 - (LAT1 - LAT0) * (np.arange(gh) + 0.5) / gh
    gx = (lons + 180.0) / 360.0 * N * 256 - tx0 * 256
    lat_r = np.radians(lats)
    gy = (1 - np.log(np.tan(lat_r) + 1 / np.cos(lat_r)) / math.pi) / 2 * N * 256 - ty0 * 256
    gxi = np.clip(gx.astype(int), 0, mos.shape[1] - 1)
    gyi = np.clip(gy.astype(int), 0, mos.shape[0] - 1)
    return mos[gyi][:, gxi]


GW = 900
GH = int(GW * (LAT1 - LAT0) / (LON1 - LON0))
H = heights(GW, GH)
tdir = os.path.join(os.path.dirname(JOB["out"]), "arazi_stil")
os.makedirs(tdir, exist_ok=True)
H.clip(0, 9000).astype("<f4").tofile(os.path.join(tdir, "heights.f32"))
(H > 1.0).astype(np.uint8).tofile(os.path.join(tdir, "land.u8"))


def norm(lat, lon):
    return (lon - LON0) / (LON1 - LON0), (LAT1 - lat) / (LAT1 - LAT0)


markers = [{"name": n, "sub": "", "x": norm(*ll)[0], "y": norm(*ll)[1]} for n, ll in JOB["places"].items()]
json.dump({"w": GW, "h": GH, "bbox": [LON0, LAT0, LON1, LAT1], "markers": markers}, open(os.path.join(tdir, "meta.json"), "w"))

# --------------------------------------------------------------- reuse the shot renderer
shot_job = {"concept": "bakir", "shot": "map", "width": JOB.get("width", 1920), "height": JOB.get("height", 1080), "fps": 30,
            "seconds": 2, "terrain": tdir, "quality": "final", "relief": JOB.get("relief", 0.65)}
sp = os.path.join(os.path.dirname(JOB["out"]), "_shot.json")
json.dump(shot_job, open(sp, "w"))
sys.argv = [sys.argv[0], "--", sp]
sys.path.insert(0, HERE)
import crstudio_shot as cs  # noqa: E402

sc = cs.reset_scene()
hts, land, meta = cs.load_terrain()
terr = cs.build_terrain(hts, land, meta)


def world_xy_nodes(nt):
    """Vector socket mapping world XY onto the Blue Marble crop (0..1)."""
    geo = nt.nodes.new("ShaderNodeNewGeometry")
    sep = nt.nodes.new("ShaderNodeSeparateXYZ")
    nt.links.new(geo.outputs["Position"], sep.inputs[0])
    mu = nt.nodes.new("ShaderNodeMath")
    mu.operation = "MULTIPLY_ADD"
    mu.inputs[1].default_value = 1.0 / cs.SX
    mu.inputs[2].default_value = 0.5
    nt.links.new(sep.outputs["X"], mu.inputs[0])
    mv = nt.nodes.new("ShaderNodeMath")
    mv.operation = "MULTIPLY_ADD"
    mv.inputs[1].default_value = 1.0 / cs.SY
    mv.inputs[2].default_value = 0.5
    nt.links.new(sep.outputs["Y"], mv.inputs[0])
    com = nt.nodes.new("ShaderNodeCombineXYZ")
    nt.links.new(mu.outputs[0], com.inputs[0])
    nt.links.new(mv.outputs[0], com.inputs[1])
    return com.outputs[0]


BM = bpy.data.images.load(JOB["bm"])


def satellite_material(name, karma=False, sea=False):
    m = bpy.data.materials.new(name)
    m.use_nodes = True
    nt = m.node_tree
    bsdf = nt.nodes.get("Principled BSDF")
    bsdf.inputs["Roughness"].default_value = 0.85 if not sea else 0.55
    for key in ("Specular IOR Level", "Specular"):
        if key in bsdf.inputs:
            bsdf.inputs[key].default_value = 0.25 if not sea else 0.12
    tex = nt.nodes.new("ShaderNodeTexImage")
    tex.image = BM
    tex.extension = "EXTEND"
    nt.links.new(world_xy_nodes(nt), tex.inputs["Vector"])
    bc = nt.nodes.new("ShaderNodeBrightContrast")
    bc.inputs["Bright"].default_value = -0.04
    bc.inputs["Contrast"].default_value = 0.12
    nt.links.new(tex.outputs["Color"], bc.inputs["Color"])
    hs = nt.nodes.new("ShaderNodeHueSaturation")
    hs.inputs["Saturation"].default_value = 0.35 if karma else 0.95
    hs.inputs["Value"].default_value = 1.0 if not sea else 1.25
    nt.links.new(bc.outputs["Color"], hs.inputs["Color"])
    last = hs.outputs["Color"]
    if karma:
        # bronze tint: multiply the desaturated photo with copper
        mix = nt.nodes.new("ShaderNodeMix")
        mix.data_type = "RGBA"
        mix.blend_type = "MULTIPLY"
        mix.inputs[0].default_value = 0.85
        nt.links.new(last, mix.inputs[6])
        mix.inputs[7].default_value = (1.0, 0.62, 0.32, 1) if not sea else (0.35, 0.5, 0.75, 1)
        last = mix.outputs[2]
    nt.links.new(last, bsdf.inputs["Base Color"])
    return m


if BASE in ("uydu", "karma"):
    terr.data.materials.clear()
    terr.data.materials.append(satellite_material("Sat", karma=BASE == "karma"))
    for ob in bpy.data.objects:
        if ob.name.startswith("Plane"):
            ob.data.materials.clear()
            ob.data.materials.append(satellite_material("SatSea", karma=BASE == "karma", sea=True))
    if BASE == "uydu":
        coast = bpy.data.objects.get("Coast")
        if coast:
            bpy.data.objects.remove(coast)

# lights: low warm sun for drama
if BASE == "bakir":
    cs.lights()
    for ob in list(sc.objects):
        if ob.type == "LIGHT":
            c = ob.constraints.new("TRACK_TO")
            c.target = terr
else:
    sun = bpy.data.lights.new("Sun", "SUN")
    sun.energy = 3.6
    sun.angle = math.radians(2)
    sun.color = (1.0, 0.93, 0.84)
    ob = bpy.data.objects.new("Sun", sun)
    # classic cartographic light from the north-west, fairly high: relief reads, little black shade
    ob.rotation_euler = (math.radians(42), 0, math.radians(-135))
    sc.collection.objects.link(ob)
    # bright sky for the lighting, near-black for the camera (no horizon, no grey sky)
    wnt = sc.world.node_tree
    bg = wnt.nodes["Background"]
    bg.inputs[0].default_value = (0.55, 0.62, 0.75, 1)
    bg.inputs[1].default_value = 0.9
    dark = wnt.nodes.new("ShaderNodeBackground")
    dark.inputs[0].default_value = (0.004, 0.006, 0.012, 1)
    lp = wnt.nodes.new("ShaderNodeLightPath")
    mixw = wnt.nodes.new("ShaderNodeMixShader")
    wnt.links.new(lp.outputs["Is Camera Ray"], mixw.inputs[0])
    wnt.links.new(bg.outputs[0], mixw.inputs[1])
    wnt.links.new(dark.outputs[0], mixw.inputs[2])
    wnt.links.new(mixw.outputs[0], wnt.nodes["World Output"].inputs[0])
    try:
        sc.view_settings.view_transform = "Standard"
        sc.view_settings.look = "None"
    except TypeError:
        pass

# --------------------------------------------------------------- camera per frame


def W(lat, lon):
    x, y = norm(lat, lon)
    return cs.to_world(x, y)


P = JOB["places"]
if FRAME == "acilis":
    tx, ty = W(38.3, 30.2)
    dist, tilt, yaw, lens = 5.6, 40.0, -20.0, 30
elif FRAME == "rota":
    ax, ay = W(*P["Efes"])
    bx, by = W(*P["Tarsus"])
    tx, ty = (ax + bx) / 2, (ay + by) / 2 + 0.55
    dist, tilt, yaw, lens = 9.5, 58.0, 0.0, 35
else:
    pts = [W(*P[k]) for k in ("Efes", "Tarsus", "Afşin")]
    tx = sum(p[0] for p in pts) / 3
    ty = sum(p[1] for p in pts) / 3 - 0.2
    dist, tilt, yaw, lens = 10.5, 66.0, 6.0, 35

cam_d = bpy.data.cameras.new("Cam")
cam_d.lens = lens
cam_d.clip_end = 300
cam = bpy.data.objects.new("Cam", cam_d)
sc.collection.objects.link(cam)
sc.camera = cam
tgt = bpy.data.objects.new("T", None)
sc.collection.objects.link(tgt)
tgt.location = (tx, ty, 0.1)
a, t = math.radians(yaw), math.radians(tilt)
cam.location = (tx + dist * math.sin(a) * math.cos(t), ty - dist * math.cos(a) * math.cos(t), dist * math.sin(t))
tc = cam.constraints.new("TRACK_TO")
tc.target = tgt
sc.render.resolution_x, sc.render.resolution_y = JOB.get("width", 1920), JOB.get("height", 1080)
if hasattr(sc.eevee, "taa_render_samples"):
    sc.eevee.taa_render_samples = 48
sc.frame_set(1)
bpy.context.view_layer.update()

from bpy_extras.object_utils import world_to_camera_view  # noqa: E402
from mathutils import Vector  # noqa: E402

screen = {}
for name, (la, lo) in P.items():
    x, y = norm(la, lo)
    if not (0 <= x <= 1 and 0 <= y <= 1):
        continue
    wx, wy = cs.to_world(x, y)
    co = world_to_camera_view(sc, cam, Vector((wx, wy, cs.height_at(x, y))))
    screen[name] = [co.x, 1 - co.y, co.z]
json.dump(screen, open(os.path.splitext(JOB["out"])[0] + ".json", "w"), ensure_ascii=False)

sc.render.image_settings.file_format = "PNG"
sc.render.filepath = JOB["out"]
bpy.ops.render.render(write_still=True)
print("[stil] done", JOB["out"], screen)
