"""CrStudio Blender shot renderer (runs headless: no window).

    blender -b --factory-startup --python crstudio_shot.py -- shot.json

`shot.json` (written by CrStudio, `crates/engine/src/crstudio/blender.rs`):
    concept   "bakir" | "minyatur" | "diorama"      the channel's visual identity
    shot      "map" | "route" | "intro" | "chapter"
    width, height, fps, seconds
    out       output movie path (.mov ProRes 422 HQ; falls back to .mp4 H.264)
    title, subtitle, kicker                          texts (Turkish)
    terrain   folder with meta.json + heights.f32 + land.u8 (from CrStudio; real elevation),
              or "" for a procedural terrain (intro / chapter backgrounds)
    font, font_bold                                  TTF/OTF paths ("" = Blender's own font)
    quality   "draft" | "final"

Original work for CrStudio (personal use). Runs inside Blender; nothing here is linked into the
CrStudio binary.
"""

import json
import math
import os
import sys

import bpy
from mathutils import Matrix
import numpy as np

ARGS = sys.argv[sys.argv.index("--") + 1 :] if "--" in sys.argv else []
P = json.load(open(ARGS[0], encoding="utf-8")) if ARGS else {}
CONCEPT = P.get("concept", "bakir")
SHOT = P.get("shot", "map")
W, H = int(P.get("width", 1920)), int(P.get("height", 1080))
FPS = int(P.get("fps", 30))
SECONDS = float(P.get("seconds", 6.0))
FRAMES = max(2, int(round(SECONDS * FPS)))
ASPECT = W / H
# world size of the terrain plane: 16 units wide, height by the frame aspect
SX = 16.0
SY = SX / ASPECT if ASPECT >= 1 else SX * 1.0
DRAFT = P.get("quality", "final") == "draft"

PALETTES = {
    # Kadim Atlas: satellite relief in a bronze tone, near-black sea, one amber accent, cream type
    "atlas": {
        "sea": (0.004, 0.008, 0.015), "sea2": (0.02, 0.03, 0.045),
        "low": (0.10, 0.07, 0.045), "high": (0.78, 0.60, 0.40),
        # linear values (the atlas renders with the Standard view): amber ≈ #F2A22E, cream ≈ #F5E9D0
        "line": (0.887, 0.361, 0.027), "text": (0.913, 0.815, 0.630), "accent": (0.887, 0.361, 0.027),
        "bg": (0.004, 0.005, 0.008),
    },
    "bakir": {
        "sea": (0.002, 0.005, 0.010), "sea2": (0.020, 0.035, 0.050),
        "low": (0.035, 0.020, 0.010), "high": (1.0, 0.62, 0.28),
        "line": (1.0, 0.58, 0.16), "text": (1.0, 0.90, 0.68), "accent": (0.95, 0.52, 0.14),
        "bg": (0.002, 0.003, 0.005), "cool": (0.35, 0.55, 1.0),
    },
    "minyatur": {
        "sea": (0.27, 0.50, 0.53), "sea2": (0.55, 0.72, 0.70),
        "low": (0.87, 0.74, 0.53), "high": (0.50, 0.26, 0.15),
        "line": (0.55, 0.08, 0.08), "text": (0.25, 0.10, 0.06), "accent": (0.70, 0.52, 0.20),
        "bg": (0.93, 0.88, 0.76),
    },
    "diorama": {
        "sea": (0.08, 0.30, 0.36), "sea2": (0.25, 0.55, 0.60),
        "low": (0.55, 0.47, 0.36), "high": (0.86, 0.79, 0.66),
        "line": (0.90, 0.12, 0.18), "text": (0.98, 0.96, 0.92), "accent": (0.90, 0.12, 0.18),
        "bg": (0.035, 0.030, 0.027),
    },
}
PAL = PALETTES.get(CONCEPT, PALETTES["bakir"])


def log(*a):
    print("[crstudio]", *a, flush=True)


# ------------------------------------------------------------------ scene setup
def reset_scene():
    bpy.ops.wm.read_factory_settings(use_empty=True)
    sc = bpy.context.scene
    sc.render.resolution_x, sc.render.resolution_y = W, H
    sc.render.resolution_percentage = 100
    sc.render.fps = FPS
    sc.frame_start, sc.frame_end = 1, FRAMES
    for eng in ("BLENDER_EEVEE_NEXT", "BLENDER_EEVEE"):
        try:
            sc.render.engine = eng
            break
        except TypeError:
            continue
    ee = sc.eevee
    # final = 24 samples without ray tracing: the map look is matte (no mirror sea), and 64 samples
    # with ray tracing cost ~6x the time on a laptop for no visible gain
    for attr, val in (("taa_render_samples", 12 if DRAFT else 24), ("use_gtao", True), ("use_bloom", True),
                      ("use_shadows", True), ("use_raytracing", False)):
        if hasattr(ee, attr):
            try:
                setattr(ee, attr, val)
            except Exception:
                pass
    sc.view_settings.view_transform = "AgX" if "AgX" in [i.identifier for i in sc.view_settings.bl_rna.properties["view_transform"].enum_items] else "Filmic"
    sc.view_settings.look = "None"
    if CONCEPT == "bakir":
        for look in ("AgX - High Contrast", "AgX - Punchy", "High Contrast"):
            try:
                sc.view_settings.look = look
                break
            except TypeError:
                continue
    world = bpy.data.worlds.new("W")
    sc.world = world
    world.use_nodes = True
    bg = world.node_tree.nodes.get("Background")
    bg.inputs[0].default_value = (*PAL["bg"], 1)
    bg.inputs[1].default_value = 0.35 if CONCEPT != "minyatur" else 1.0
    if CONCEPT == "atlas":
        sc.view_settings.view_transform = "Standard"
        sc.view_settings.look = "None"
        # a bright sky lights the land; the camera sees near-black (no horizon, no grey sky)
        wnt = world.node_tree
        bg.inputs[0].default_value = (0.55, 0.62, 0.75, 1)
        bg.inputs[1].default_value = 0.9
        dark = wnt.nodes.new("ShaderNodeBackground")
        dark.inputs[0].default_value = (*PAL["bg"], 1)
        lp = wnt.nodes.new("ShaderNodeLightPath")
        mixw = wnt.nodes.new("ShaderNodeMixShader")
        wnt.links.new(lp.outputs["Is Camera Ray"], mixw.inputs[0])
        wnt.links.new(bg.outputs[0], mixw.inputs[1])
        wnt.links.new(dark.outputs[0], mixw.inputs[2])
        wnt.links.new(mixw.outputs[0], wnt.nodes["World Output"].inputs[0])
    return sc


def material(name, base=None, rough=0.6, metal=0.0, emission=None, strength=0.0, attr=None, ramp=None):
    m = bpy.data.materials.new(name)
    m.use_nodes = True
    nt = m.node_tree
    bsdf = nt.nodes.get("Principled BSDF")
    bsdf.inputs["Roughness"].default_value = rough
    bsdf.inputs["Metallic"].default_value = metal
    if base:
        bsdf.inputs["Base Color"].default_value = (*base, 1)
    if attr and ramp:
        an = nt.nodes.new("ShaderNodeAttribute")
        an.attribute_name = attr
        cr = nt.nodes.new("ShaderNodeValToRGB")
        cr.color_ramp.elements[0].color = (*ramp[0], 1)
        cr.color_ramp.elements[1].color = (*ramp[1], 1)
        for pos, col in ramp[2:]:
            e = cr.color_ramp.elements.new(pos)
            e.color = (*col, 1)
        nt.links.new(an.outputs["Fac"], cr.inputs["Fac"])
        nt.links.new(cr.outputs["Color"], bsdf.inputs["Base Color"])
    if emission:
        key = "Emission Color" if "Emission Color" in bsdf.inputs else "Emission"
        bsdf.inputs[key].default_value = (*emission, 1)
        bsdf.inputs["Emission Strength"].default_value = strength
    return m


# ------------------------------------------------------------------ terrain data
def load_terrain():
    d = P.get("terrain") or ""
    meta_p = os.path.join(d, "meta.json")
    if d and os.path.isfile(meta_p):
        meta = json.load(open(meta_p, encoding="utf-8"))
        w, h = int(meta["w"]), int(meta["h"])
        hts = np.fromfile(os.path.join(d, "heights.f32"), dtype="<f4", count=w * h).reshape(h, w)
        land = np.fromfile(os.path.join(d, "land.u8"), dtype=np.uint8, count=w * h).reshape(h, w) > 0
        return hts, land, meta
    # procedural stand-in (intro / chapter backdrops)
    w, h = 400, int(400 / ASPECT)
    rng = np.random.default_rng(int(P.get("seed", 7)))
    acc = np.zeros((h, w), np.float32)
    amp = 1.0
    for o in range(6):
        c = 3 * 2 ** o
        g = rng.random((max(2, c * h // w) + 2, c + 2)).astype(np.float32)
        ys = np.linspace(0, g.shape[0] - 2, h)
        xs = np.linspace(0, g.shape[1] - 2, w)
        y0, x0 = ys.astype(int), xs.astype(int)
        fy, fx = (ys - y0)[:, None], (xs - x0)[None, :]
        v = (g[y0][:, x0] * (1 - fy) * (1 - fx) + g[y0 + 1][:, x0] * fy * (1 - fx) + g[y0][:, x0 + 1] * (1 - fy) * fx + g[y0 + 1][:, x0 + 1] * fy * fx)
        acc += amp * v
        amp *= 0.5
    acc /= 1.97
    xs = np.linspace(0, 1, w)[None, :]
    elev = (xs - 0.35) * 1.4 + (acc - 0.5) * 1.2
    land = elev > 0
    hts = np.clip(elev, 0, None) * 1800.0
    return hts.astype(np.float32), land, {"w": w, "h": h, "markers": [], "route": []}


def to_world(x, y):
    """Normalized (0..1, y down) → world XY."""
    return (x - 0.5) * SX, (0.5 - y) * SY


def merc(lat):
    return math.log(math.tan(math.radians(lat)) + 1 / math.cos(math.radians(lat)))


def inv_merc(y):
    return np.degrees(np.arctan(np.sinh(y)))


def imagery():
    """(path, [lon0, lat0, lon1, lat1]) of the satellite mosaic CrStudio prepared, or None."""
    p = P.get("imagery") or ""
    bb = P.get("imagery_bbox") or []
    if p and os.path.isfile(p) and len(bb) == 4:
        return p, [float(x) for x in bb]
    return None


def tr_upper(s):
    """Turkish-aware upper case (i → İ, ı → I)."""
    return s.replace("i", "İ").replace("ı", "I").upper()


def blur(a, sigma):
    """Separable Gaussian blur (numpy only; Blender's Python has no scipy)."""
    if sigma <= 0:
        return a
    r = max(1, int(sigma * 3))
    k = np.exp(-0.5 * (np.arange(-r, r + 1) / sigma) ** 2)
    k /= k.sum()
    p = np.pad(a, ((0, 0), (r, r)), mode="edge")
    a = sum(k[i] * p[:, i : i + a.shape[1]] for i in range(2 * r + 1))
    p = np.pad(a, ((r, r), (0, 0)), mode="edge")
    return sum(k[i] * p[i : i + a.shape[0], :] for i in range(2 * r + 1))


def upsample(a, f):
    """Bilinear upsampling by an integer factor."""
    h, w = a.shape
    ys = np.linspace(0, h - 1, h * f)
    xs = np.linspace(0, w - 1, w * f)
    y0 = np.floor(ys).astype(int)
    x0 = np.floor(xs).astype(int)
    y1 = np.minimum(y0 + 1, h - 1)
    x1 = np.minimum(x0 + 1, w - 1)
    fy = (ys - y0)[:, None]
    fx = (xs - x0)[None, :]
    return (a[y0][:, x0] * (1 - fy) * (1 - fx) + a[y1][:, x0] * fy * (1 - fx) + a[y0][:, x1] * (1 - fy) * fx + a[y1][:, x1] * fy * fx)


def prepare_heights(hts, land, meta):
    """Smooth the sampled elevation (coarse tiles alias into spikes), soften the coast and
    scale the relief to the map's extent: a 60°-wide map must not look like the Alps."""
    f = 2 if max(hts.shape) < 500 else 1
    landf = upsample(land.astype(np.float32), f)
    hts = upsample(np.where(land, hts, 0.0).astype(np.float32), f)
    landf = blur(landf, 0.9 * f)
    land = landf > 0.5
    hts = blur(hts, 1.1 * f)
    bbox = meta.get("bbox") or [0, 0, 1, 1]
    span = abs(float(bbox[2]) - float(bbox[0])) or 1.0
    extent = max(0.28, min(1.0, 1.0 / (1.0 + span / 10.0)))
    extent *= max(0.2, min(4.0, float(P.get("relief", 1.0))))
    return hts, land, landf, extent


def build_terrain(hts, land, meta):
    hts, land, landf, extent = prepare_heights(hts, land, meta)
    h, w = hts.shape
    hmax = float(max(1.0, np.percentile(hts[land], 99.5) if land.any() else 1.0))
    relief = {"bakir": 1.0, "minyatur": 0.6, "diorama": 1.3, "atlas": 0.7}.get(CONCEPT, 1.0) * extent
    hn0 = np.clip(hts / hmax, 0, 1.2)
    # gentle curve: lowlands still read, peaks don't spike
    z = np.power(hn0, 0.75) * relief
    if CONCEPT == "minyatur":
        # paper terraces
        steps = 6
        z = np.floor(z / relief * steps) / steps * relief
    # slope down into the sea instead of a cliff; the sea floor sits under the water plane
    # (coplanar faces z-fight into stripes)
    k = np.clip((landf - 0.42) * 5.0, 0.0, 1.0)
    z = (z + 0.03) * k - 0.04 * (1.0 - k)
    xs = np.linspace(-SX / 2, SX / 2, w)
    ys = np.linspace(SY / 2, -SY / 2, h)
    X, Y = np.meshgrid(xs, ys)
    verts = np.stack([X.ravel(), Y.ravel(), z.ravel()], 1)
    faces = []
    idx = np.arange(w * h).reshape(h, w)
    a = idx[:-1, :-1].ravel()
    b = idx[:-1, 1:].ravel()
    c = idx[1:, 1:].ravel()
    dd = idx[1:, :-1].ravel()
    faces = np.stack([a, b, c, dd], 1)
    me = bpy.data.meshes.new("Terrain")
    me.vertices.add(len(verts))
    me.vertices.foreach_set("co", verts.astype(np.float32).ravel())
    me.loops.add(len(faces) * 4)
    me.loops.foreach_set("vertex_index", faces.astype(np.int32).ravel())
    me.polygons.add(len(faces))
    me.polygons.foreach_set("loop_start", np.arange(0, len(faces) * 4, 4, dtype=np.int32))
    me.polygons.foreach_set("loop_total", np.full(len(faces), 4, dtype=np.int32))
    me.update(calc_edges=True)
    me.validate()
    for p in me.polygons:
        p.use_smooth = True
    # attributes: height (0..1) and coast/contour lines
    hn = (np.clip(hts / hmax, 0, 1) * land).astype(np.float32)
    att = me.attributes.new("hgt", "FLOAT", "POINT")
    att.data.foreach_set("value", hn.ravel())
    # engraved contour lines only (the coast is a real curve, see coastline())
    band = np.abs(np.sin(np.power(hn, 0.75) * 22.0))
    contour = np.clip(1.0 - band / 0.10, 0.0, 1.0) * (hn > 0.06) * land
    att2 = me.attributes.new("line", "FLOAT", "POINT")
    att2.data.foreach_set("value", (contour * (0.30 if extent > 0.5 else 0.0)).astype(np.float32).ravel())
    if CONCEPT == "atlas" and imagery() and meta.get("bbox"):
        # where each vertex falls on the satellite mosaic (meta rows are Mercator-linear)
        _, ib = imagery()
        bb = meta.get("bbox") or [0, 0, 1, 1]
        lon = np.linspace(float(bb[0]), float(bb[2]), w)
        my_top, my_bot = merc(float(bb[3])), merc(float(bb[1]))
        lat = inv_merc(np.linspace(my_top, my_bot, h))
        LON, LAT = np.meshgrid(lon, lat)
        u = (LON - ib[0]) / (ib[2] - ib[0])
        v = (LAT - ib[1]) / (ib[3] - ib[1])
        inside = ((u >= 0) & (u <= 1) & (v >= 0) & (v <= 1)).astype(np.float32)
        att3 = me.attributes.new("llu", "FLOAT_VECTOR", "POINT")
        att3.data.foreach_set("vector", np.stack([u, v, inside], -1).astype(np.float32).ravel())
    ob = bpy.data.objects.new("Terrain", me)
    bpy.context.scene.collection.objects.link(ob)
    ob.data.materials.append(terrain_material())
    if CONCEPT != "diorama":
        coastline(landf, z)
    ob["zgrid"] = 1  # marker for callers
    TERRAIN_Z["z"] = z
    # sea plane
    bpy.ops.mesh.primitive_plane_add(size=1, location=(0, 0, 0.0))
    sea = bpy.context.active_object
    sea.scale = (SX * 1.6, SY * 1.6, 1)
    sea.data.materials.append(sea_material())
    if CONCEPT == "diorama":
        # the slab: solidify the terrain, wooden plinth under it
        sea.scale = (SX, SY, 1)
        bpy.ops.mesh.primitive_cube_add(size=1, location=(0, 0, -0.256))
        base = bpy.context.active_object
        base.scale = (SX * 1.02, SY * 1.02, 0.5)
        base.data.materials.append(material("Wood", (0.20, 0.11, 0.06), 0.45))
        bpy.ops.mesh.primitive_plane_add(size=1, location=(0, 0, -0.71))
        table = bpy.context.active_object
        table.scale = (80, 80, 1)
        table.data.materials.append(material("Table", (0.06, 0.045, 0.035), 0.7))
    return ob


TERRAIN_Z = {}
LINK_TO = None

# marching-squares: case -> list of (edge, edge); edges 0 top, 1 right, 2 bottom, 3 left
_MS = {
    1: [(3, 2)], 2: [(2, 1)], 3: [(3, 1)], 4: [(0, 1)], 5: [(3, 0), (2, 1)], 6: [(0, 2)], 7: [(3, 0)],
    8: [(0, 3)], 9: [(0, 2)], 10: [(0, 1), (2, 3)], 11: [(0, 1)], 12: [(1, 3)], 13: [(1, 2)], 14: [(2, 3)],
}


def coastline(landf, z):
    """The coast as a real smooth curve (gold wire / ink line) traced at the 0.5 level."""
    h, w = landf.shape
    v = landf - 0.5
    a, b, c, d = v[:-1, :-1], v[:-1, 1:], v[1:, 1:], v[1:, :-1]  # tl, tr, br, bl
    case = (a > 0) * 8 + (b > 0) * 4 + (c > 0) * 2 + (d > 0) * 1
    ii, jj = np.nonzero((case > 0) & (case < 15))

    def lerp(v0, v1):
        den = v0 - v1
        return 0.5 if abs(den) < 1e-9 else float(v0 / den)

    verts, index, edges = [], {}, []

    def pt(i, j, e):
        if e == 0:
            x, y = j + lerp(v[i, j], v[i, j + 1]), i
        elif e == 1:
            x, y = j + 1, i + lerp(v[i, j + 1], v[i + 1, j + 1])
        elif e == 2:
            x, y = j + lerp(v[i + 1, j], v[i + 1, j + 1]), i + 1
        else:
            x, y = j, i + lerp(v[i, j], v[i + 1, j])
        key = (round(x, 4), round(y, 4))
        k = index.get(key)
        if k is None:
            k = len(verts)
            index[key] = k
            wx = -SX / 2 + x / (w - 1) * SX
            wy = SY / 2 - y / (h - 1) * SY
            verts.append((wx, wy, 0.04))
        return k

    for i, j in zip(ii, jj):
        for e0, e1 in _MS[int(case[i, j])]:
            edges.append((pt(i, j, e0), pt(i, j, e1)))
    if not edges:
        return None
    me = bpy.data.meshes.new("Coast")
    me.from_pydata(verts, edges, [])
    ob = bpy.data.objects.new("Coast", me)
    bpy.context.scene.collection.objects.link(ob)
    for o in bpy.context.selected_objects:
        o.select_set(False)
    bpy.context.view_layer.objects.active = ob
    ob.select_set(True)
    try:
        bpy.ops.object.convert(target="CURVE")
        ob = bpy.context.view_layer.objects.active
        ob.data.bevel_depth = 0.0045 if CONCEPT == "bakir" else 0.006
        ob.data.bevel_resolution = 2
        for sp in ob.data.splines:
            sp.use_smooth = True
    except Exception as e:  # keep the render going without a coast line
        log("coast convert failed:", e)
    if CONCEPT == "atlas":
        ob.data.bevel_depth = 0.0016
        ob.data.materials.append(material("CoastM", PAL["line"], 0.4, 0.0, emission=PAL["line"], strength=0.8))
    elif CONCEPT == "bakir":
        ob.data.materials.append(material("CoastM", PAL["line"], 0.3, 1.0, emission=PAL["line"], strength=2.5))
    else:
        ob.data.materials.append(material("CoastM", PAL["line"], 0.8))
    return ob


def atlas_land_material():
    """Satellite photo, desaturated and multiplied with bronze; outside the mosaic, a bronze ramp."""
    m = material("AtlasLand", rough=0.85, attr="hgt", ramp=[PAL["low"], PAL["high"], (0.5, (0.42, 0.30, 0.19))])
    nt = m.node_tree
    bsdf = nt.nodes.get("Principled BSDF")
    for key in ("Specular IOR Level", "Specular"):
        if key in bsdf.inputs:
            bsdf.inputs[key].default_value = 0.22
    ramp_out = bsdf.inputs["Base Color"].links[0].from_socket
    im = imagery()
    if not im:
        return m
    at = nt.nodes.new("ShaderNodeAttribute")
    at.attribute_name = "llu"
    tex = nt.nodes.new("ShaderNodeTexImage")
    tex.image = bpy.data.images.load(im[0], check_existing=True)
    tex.extension = "EXTEND"
    tex.interpolation = "Cubic"
    nt.links.new(at.outputs["Vector"], tex.inputs["Vector"])
    bc = nt.nodes.new("ShaderNodeBrightContrast")
    bc.inputs["Contrast"].default_value = 0.12
    nt.links.new(tex.outputs["Color"], bc.inputs["Color"])
    hs = nt.nodes.new("ShaderNodeHueSaturation")
    hs.inputs["Saturation"].default_value = 0.35
    nt.links.new(bc.outputs["Color"], hs.inputs["Color"])
    tint = nt.nodes.new("ShaderNodeMix")
    tint.data_type = "RGBA"
    tint.blend_type = "MULTIPLY"
    tint.inputs[0].default_value = 0.85
    nt.links.new(hs.outputs["Color"], tint.inputs[6])
    tint.inputs[7].default_value = (1.0, 0.64, 0.34, 1)
    # inside the mosaic: photo; outside: the bronze ramp
    sep = nt.nodes.new("ShaderNodeSeparateXYZ")
    nt.links.new(at.outputs["Vector"], sep.inputs[0])
    pick = nt.nodes.new("ShaderNodeMix")
    pick.data_type = "RGBA"
    nt.links.new(sep.outputs["Z"], pick.inputs[0])
    nt.links.new(ramp_out, pick.inputs[6])
    nt.links.new(tint.outputs[2], pick.inputs[7])
    nt.links.new(pick.outputs[2], bsdf.inputs["Base Color"])
    return m


def terrain_material():
    if CONCEPT == "atlas":
        return atlas_land_material()
    if CONCEPT == "bakir":
        m = material("Land", rough=0.55, metal=0.45, attr="hgt", ramp=[PAL["low"], PAL["high"], (0.55, (0.36, 0.22, 0.11))])
    elif CONCEPT == "minyatur":
        m = material("Land", rough=0.9, attr="hgt",
                     ramp=[(0.86, 0.75, 0.55), (0.42, 0.50, 0.33), (0.3, (0.78, 0.60, 0.38)), (0.6, (0.62, 0.36, 0.22))])
    else:
        m = material("Clay", rough=0.75, attr="hgt", ramp=[PAL["low"], PAL["high"], (0.7, (0.45, 0.52, 0.34))])
    # gold / ink lines as emission (bakır) or dark ink (minyatür)
    nt = m.node_tree
    bsdf = nt.nodes.get("Principled BSDF")
    if CONCEPT in ("bakir", "minyatur"):
        an = nt.nodes.new("ShaderNodeAttribute")
        an.attribute_name = "line"
        mix = nt.nodes.new("ShaderNodeMix")
        mix.data_type = "RGBA"
        src = bsdf.inputs["Base Color"].links[0].from_socket if bsdf.inputs["Base Color"].is_linked else None
        if src:
            nt.links.new(src, mix.inputs[6])
        mix.inputs[7].default_value = (*PAL["line"], 1)
        nt.links.new(an.outputs["Fac"], mix.inputs[0])
        nt.links.new(mix.outputs[2], bsdf.inputs["Base Color"])
        if CONCEPT == "bakir":
            key = "Emission Color" if "Emission Color" in bsdf.inputs else "Emission"
            bsdf.inputs[key].default_value = (*PAL["line"], 1)
            mul = nt.nodes.new("ShaderNodeMath")
            mul.operation = "MULTIPLY"
            mul.inputs[1].default_value = 2.2
            nt.links.new(an.outputs["Fac"], mul.inputs[0])
            nt.links.new(mul.outputs[0], bsdf.inputs["Emission Strength"])
    return m


def sea_material():
    if CONCEPT == "atlas":
        m = material("Sea", (0.010, 0.020, 0.034), rough=0.55)
        bsdf = m.node_tree.nodes.get("Principled BSDF")
        for key in ("Specular IOR Level", "Specular"):
            if key in bsdf.inputs:
                bsdf.inputs[key].default_value = 0.15
        return m
    if CONCEPT == "bakir":
        m = material("Sea", PAL["sea"], rough=0.25, metal=0.2)
        # engraved ripple lines (wave texture) faintly in bronze
        nt = m.node_tree
        bsdf = nt.nodes.get("Principled BSDF")
        wave = nt.nodes.new("ShaderNodeTexWave")
        wave.inputs["Scale"].default_value = 9
        wave.inputs["Distortion"].default_value = 6
        if "Detail" in wave.inputs:
            wave.inputs["Detail"].default_value = 1.0
        ramp = nt.nodes.new("ShaderNodeValToRGB")
        ramp.color_ramp.elements[0].position = 0.97
        ramp.color_ramp.elements[0].color = (*PAL["sea"], 1)
        ramp.color_ramp.elements[1].color = (*PAL["sea2"], 1)
        nt.links.new(wave.outputs["Fac"], ramp.inputs["Fac"])
        nt.links.new(ramp.outputs["Color"], bsdf.inputs["Base Color"])
        return m
    if CONCEPT == "minyatur":
        return material("Sea", PAL["sea"], rough=0.95)
    return material("Sea", PAL["sea"], rough=0.08, metal=0.0)


# ------------------------------------------------------------------ objects
def text_obj(body, size, loc, col, bold=False, align="LEFT", emission=0.0, extrude=0.0, name="Text", box=0.0, display=False):
    cu = bpy.data.curves.new(name, "FONT")
    cu.body = body
    cu.size = size
    cu.align_x = align
    cu.extrude = extrude
    if box > 0:
        # wrap long lines inside a text box (width in world units)
        cu.text_boxes[0].width = box / max(size, 1e-6) * size
        cu.text_boxes[0].x = 0.0
        if align == "CENTER":
            cu.text_boxes[0].x = -box / 2
    path = (P.get("font_display") if display else "") or P.get("font_bold" if bold else "font") or ""
    if path and os.path.isfile(path):
        try:
            cu.font = bpy.data.fonts.load(path, check_existing=True)
        except Exception:
            pass
    ob = bpy.data.objects.new(name, cu)
    ob.location = loc
    (LINK_TO or bpy.context.scene.collection).objects.link(ob)
    ob.data.materials.append(material(name + "M", col, 0.5, 0.3 if CONCEPT == "bakir" else 0.0, emission=col if emission else None, strength=emission))
    return ob


def _parent(child, root):
    bpy.context.view_layer.update()
    child.parent = root
    child.matrix_parent_inverse = root.matrix_world.inverted()


def pin(x, y, z):
    if CONCEPT == "bakir":
        bpy.ops.mesh.primitive_cone_add(vertices=4, radius1=0.12, radius2=0.05, depth=0.9, location=(x, y, z))
        ob = bpy.context.active_object
        ob.data.transform(Matrix.Translation((0, 0, 0.45)))  # origin at the foot: scales into the ground
        ob.data.materials.append(material("Bronze", (0.55, 0.33, 0.14), 0.3, 1.0, emission=PAL["line"], strength=0.4))
        bpy.ops.mesh.primitive_torus_add(major_radius=0.17, minor_radius=0.008, location=(x, y, z + 0.02))
        ring = bpy.context.active_object
        ring.data.materials.append(material("Ring", PAL["line"], 0.3, 1.0, emission=PAL["line"], strength=2.5))
        _parent(ring, ob)
        return ob
    if CONCEPT == "minyatur":
        bpy.ops.mesh.primitive_cylinder_add(radius=0.09, depth=0.08, location=(x, y, z))
        ob = bpy.context.active_object
        ob.data.transform(Matrix.Translation((0, 0, 0.04)))
        ob.data.materials.append(material("Seal", PAL["line"], 0.6))
        bpy.ops.mesh.primitive_cone_add(vertices=24, radius1=0.07, radius2=0.0, depth=0.22, location=(x, y, z + 0.19))
        dome = bpy.context.active_object
        dome.data.materials.append(material("Dome", PAL["line"], 0.5))
        _parent(dome, ob)
        return ob
    bpy.ops.mesh.primitive_cylinder_add(radius=0.015, depth=1.1, location=(x, y, z))
    pole = bpy.context.active_object
    pole.data.transform(Matrix.Translation((0, 0, 0.55)))
    pole.data.materials.append(material("Pole", (0.95, 0.95, 0.95), 0.4))
    bpy.ops.mesh.primitive_plane_add(size=0.4, location=(x + 0.2, y, z + 0.95), rotation=(math.pi / 2, 0, 0))
    flag = bpy.context.active_object
    flag.scale = (1.0, 0.55, 1)
    flag.data.materials.append(material("Flag", PAL["accent"], 0.5))
    _parent(flag, pole)
    return pole


def route_curve(pts_world, start_f, end_f):
    cu = bpy.data.curves.new("Route", "CURVE")
    cu.dimensions = "3D"
    sp = cu.splines.new("POLY")
    sp.points.add(len(pts_world) - 1)
    for p, (x, y, z) in zip(sp.points, pts_world):
        p.co = (x, y, z + 0.06, 1)
    cu.bevel_depth = 0.025 if CONCEPT != "minyatur" else 0.035
    cu.use_fill_caps = True
    ob = bpy.data.objects.new("Route", cu)
    bpy.context.scene.collection.objects.link(ob)
    strength = 6.0 if CONCEPT == "bakir" else (1.5 if CONCEPT == "diorama" else 0.0)
    ob.data.materials.append(material("RouteM", PAL["line"], 0.4, 0.0, emission=PAL["line"] if strength else None, strength=strength))
    cu.bevel_factor_end = 0.0
    cu.keyframe_insert("bevel_factor_end", frame=start_f)
    cu.bevel_factor_end = 1.0
    cu.keyframe_insert("bevel_factor_end", frame=end_f)
    return ob


def height_at(x, y):
    z = TERRAIN_Z.get("z")
    if z is None:
        return 0.0
    h, w = z.shape
    i = min(h - 1, max(0, int(round(y * (h - 1)))))
    j = min(w - 1, max(0, int(round(x * (w - 1)))))
    # the highest point nearby, so a pin never sinks into a neighbouring ridge
    return float(max(0.0, z[max(0, i - 2) : i + 3, max(0, j - 2) : j + 3].max()))


def lights():
    sc = bpy.context.scene
    if CONCEPT == "atlas":
        # cartographic light from the north-west, fairly high: relief reads without black shade
        sun = bpy.data.lights.new("Sun", "SUN")
        sun.energy = 3.6
        sun.angle = math.radians(2)
        sun.color = (1.0, 0.93, 0.84)
        ob = bpy.data.objects.new("Sun", sun)
        ob.rotation_euler = (math.radians(42), 0, math.radians(-135))
        sc.collection.objects.link(ob)
        return
    if CONCEPT == "bakir":
        # warm low key light rakes the relief; a cool rim from the far side separates the ridges
        key = bpy.data.lights.new("Key", "AREA")
        key.energy = 1500
        key.color = (1.0, 0.74, 0.45)
        key.size = 5
        ob = bpy.data.objects.new("Key", key)
        ob.location = (-9, -4, 6)
        ob.rotation_euler = (math.radians(58), 0, math.radians(-65))
        sc.collection.objects.link(ob)
        rim = bpy.data.lights.new("Rim", "AREA")
        rim.energy = 700
        rim.color = PAL.get("cool", (0.4, 0.6, 1.0))
        rim.size = 8
        ob = bpy.data.objects.new("Rim", rim)
        ob.location = (6, 9, 5)
        ob.rotation_euler = (math.radians(-55), 0, math.radians(150))
        sc.collection.objects.link(ob)
    elif CONCEPT == "minyatur":
        sun = bpy.data.lights.new("Sun", "SUN")
        sun.energy = 3.2
        sun.angle = math.radians(8)
        ob = bpy.data.objects.new("Sun", sun)
        ob.rotation_euler = (math.radians(35), math.radians(20), math.radians(-35))
        sc.collection.objects.link(ob)
    else:
        for name, loc, en, col in (("Key", (-6, -6, 10), 1500, (1.0, 0.9, 0.78)), ("Rim", (8, 8, 6), 600, (0.7, 0.8, 1.0))):
            l = bpy.data.lights.new(name, "AREA")
            l.energy = en
            l.size = 5
            l.color = col
            ob = bpy.data.objects.new(name, l)
            ob.location = loc
            sc.collection.objects.link(ob)
            tr = ob.constraints.new("TRACK_TO")
            tr.target = bpy.data.objects.get("Terrain")


def _ease(ob, interpolation="BEZIER"):
    ad = ob.animation_data
    if ad and ad.action:
        for fc in getattr(ad.action, "fcurves", []):
            for kp in fc.keyframe_points:
                kp.interpolation = interpolation
                kp.easing = "EASE_IN_OUT"


def camera(target_xy, start_dist, end_dist, tilt, target_from=None, hold=0.85):
    """Orbit-free documentary move: a slow push from `start_dist` to `end_dist`, the aim gliding
    from `target_from` (e.g. the centre of all places) to `target_xy`. The move ends at `hold`
    of the shot so the last beat rests."""
    sc = bpy.context.scene
    cam_d = bpy.data.cameras.new("Cam")
    cam_d.lens = 50
    cam_d.clip_end = 400
    cam = bpy.data.objects.new("Cam", cam_d)
    sc.collection.objects.link(cam)
    sc.camera = cam
    tgt = bpy.data.objects.new("Target", None)
    sc.collection.objects.link(tgt)
    tr = cam.constraints.new("TRACK_TO")
    tr.target = tgt
    start = target_from or target_xy
    f_end = max(2, int(FRAMES * hold))
    for f, (tx, ty), dist, yaw in ((1, start, start_dist, -5), (f_end, target_xy, end_dist, 3)):
        tgt.location = (tx, ty, 0.2)
        tgt.keyframe_insert("location", frame=f)
        a = math.radians(yaw)
        t = math.radians(tilt)
        cam.location = (tx + dist * math.sin(a) * math.cos(t), ty - dist * math.cos(a) * math.cos(t), dist * math.sin(t))
        cam.keyframe_insert("location", frame=f)
    if CONCEPT == "diorama":
        cam_d.dof.use_dof = True
        cam_d.dof.focus_object = tgt
        cam_d.dof.aperture_fstop = 2.0
    _ease(cam)
    _ease(tgt)
    return cam


OVERLAY = {}


def overlay_scene(main):
    """A second, static scene for flat screen graphics (captions). The compositor lays it over
    the 3D picture, so captions are never hidden by a ridge nor blurred by the diorama's depth of
    field, and they sit exactly where a designer would put them."""
    if "scene" in OVERLAY:
        return OVERLAY["scene"], OVERLAY["cam"]
    ov = bpy.data.scenes.new("Overlay")
    ov.render.engine = main.render.engine
    ov.render.resolution_x, ov.render.resolution_y = W, H
    ov.render.resolution_percentage = 100
    ov.render.fps = FPS
    ov.frame_start, ov.frame_end = main.frame_start, main.frame_end
    ov.render.film_transparent = True
    ov.view_settings.view_transform = "Standard"
    ov.view_settings.look = "None"
    if hasattr(ov.eevee, "taa_render_samples"):
        ov.eevee.taa_render_samples = 16
    wd = bpy.data.worlds.new("OW")
    wd.use_nodes = True
    wd.node_tree.nodes["Background"].inputs[1].default_value = 0.0
    ov.world = wd
    cd = bpy.data.cameras.new("OCam")
    cd.type = "ORTHO"
    cd.ortho_scale = 1.0  # frame is 1.0 wide (landscape) / 1.0 tall (portrait)
    cam = bpy.data.objects.new("OCam", cd)
    ov.collection.objects.link(cam)
    cam.location = (0, 0, 5)
    ov.camera = cam
    # compositor: picture + overlay
    main.use_nodes = True
    nt = main.node_tree
    for n in list(nt.nodes):
        nt.nodes.remove(n)
    rl = nt.nodes.new("CompositorNodeRLayers")
    rl.scene = main
    ro = nt.nodes.new("CompositorNodeRLayers")
    ro.scene = ov
    ao = nt.nodes.new("CompositorNodeAlphaOver")
    if hasattr(ao, "use_premultiply"):
        ao.use_premultiply = False
    comp = nt.nodes.new("CompositorNodeComposite")
    nt.links.new(rl.outputs["Image"], ao.inputs[1])
    nt.links.new(ro.outputs["Image"], ao.inputs[2])
    nt.links.new(ao.outputs["Image"], comp.inputs["Image"])
    OVERLAY.update(scene=ov, cam=cam)
    return ov, cam


def flat(name, color, alpha=1.0):
    """Unlit material (exact colour in the overlay)."""
    m = bpy.data.materials.new(name)
    m.use_nodes = True
    nt = m.node_tree
    for n in list(nt.nodes):
        nt.nodes.remove(n)
    em = nt.nodes.new("ShaderNodeEmission")
    em.inputs["Color"].default_value = (*color, 1)
    out = nt.nodes.new("ShaderNodeOutputMaterial")
    if alpha < 1.0:
        tr = nt.nodes.new("ShaderNodeBsdfTransparent")
        mix = nt.nodes.new("ShaderNodeMixShader")
        mix.inputs[0].default_value = alpha
        nt.links.new(tr.outputs[0], mix.inputs[1])
        nt.links.new(em.outputs[0], mix.inputs[2])
        nt.links.new(mix.outputs[0], out.inputs["Surface"])
        if hasattr(m, "surface_render_method"):
            m.surface_render_method = "BLENDED"
        elif hasattr(m, "blend_method"):
            m.blend_method = "BLEND"
    else:
        nt.links.new(em.outputs[0], out.inputs["Surface"])
    return m


def srgb(c):
    """Palette colours are written as display values; emission wants linear."""
    return tuple(x ** 2.2 for x in c)


def lower_third(cam, kicker, title, sub, f0):
    """Caption card, bottom-left, in the flat overlay scene."""
    global LINK_TO
    ov, ocam = overlay_scene(bpy.context.scene)
    LINK_TO = ov.collection
    fw, fh = (1.0, 1.0 / ASPECT) if ASPECT >= 1 else (ASPECT, 1.0)
    rig = bpy.data.objects.new("Caption", None)
    ov.collection.objects.link(rig)
    rig.location = (-fw / 2 + fw * 0.06, -fh / 2 + fh * 0.12, 0)
    size = fw * (0.046 if ASPECT >= 1 else 0.082)
    box = fw * 0.8
    light = CONCEPT == "minyatur"
    ink = srgb((0.22, 0.10, 0.06)) if light else srgb((0.98, 0.93, 0.82))
    acc = srgb((0.62, 0.12, 0.10)) if light else srgb(tuple(min(1.0, x * 1.15) for x in PAL["accent"]))
    lines = []
    y = 0.0
    if sub:
        lines.append(("Sub", sub, size * 0.40, y, ink, False, box * 0.62, f0 + 10))
        y += size * 0.95
    rule_y = y
    y += size * 0.30
    lines.append(("Title", title, size, y, ink, True, 0.0, f0 + 3))
    y += size * 1.12
    if kicker:
        lines.append(("Kicker", kicker, size * 0.36, y, acc, True, 0.0, f0))
    # soft backdrop so the caption reads on any map
    bd = bpy.data.meshes.new("Back")
    pad = size * 0.9
    est = max(len(title) * size * 0.56, len(kicker or "") * size * 0.36 * 0.62, min(box * 0.62, len(sub or "") * size * 0.40 * 0.52))
    bw, bh = min(fw * 0.66, est + pad * 1.2), y + size * 0.55 + pad
    bd.from_pydata([(-pad, -pad - size * 0.2, -0.1), (bw, -pad - size * 0.2, -0.1), (bw, bh - pad, -0.1), (-pad, bh - pad, -0.1)], [], [(0, 1, 2, 3)])
    back = bpy.data.objects.new("Back", bd)
    ov.collection.objects.link(back)
    back.parent = rig
    back.data.materials.append(flat("BackM", (0.96, 0.92, 0.84) if light else (0.0, 0.0, 0.0), 0.55 if light else 0.42))
    fade(back, f0, f0 + 8)
    for name, body, sz, yy, col, bold, bx, fi in lines:
        t = text_obj(body, sz, (0, yy, 0), col, bold=bold, name=name, box=bx, display=(name == "Title"))
        t.data.materials.clear()
        t.data.materials.append(flat(name + "F", col))
        t.parent = rig
        fade(t, fi, fi + 12)
    rm = bpy.data.meshes.new("Rule")
    rw, rh = size * 7.0, size * 0.05
    rm.from_pydata([(0, rule_y, 0), (rw, rule_y, 0), (rw, rule_y + rh, 0), (0, rule_y + rh, 0)], [], [(0, 1, 2, 3)])
    rule = bpy.data.objects.new("Rule", rm)
    ov.collection.objects.link(rule)
    rule.parent = rig
    rule.data.materials.append(flat("RuleF", acc))
    fade(rule, f0 + 2, f0 + 12)
    LINK_TO = None
    return rig


def fade(ob, f0, f1):
    """Fade an object in by scaling it from 0 (works in every engine)."""
    s = ob.scale.copy()
    ob.scale = (0.001, 0.001, 0.001)
    ob.keyframe_insert("scale", frame=f0)
    ob.scale = s
    ob.keyframe_insert("scale", frame=f1)


# ------------------------------------------------------------------ shots
def short_place(name):
    """'YEDİ UYURLAR MAĞARASI, PANAYIRDAĞI, EFES' -> ('YEDİ UYURLAR MAĞARASI', 'PANAYIRDAĞI · EFES')."""
    import re

    name = re.sub(r"\s*\([^)]*\)", "", name or "").strip()
    parts = [p.strip() for p in name.split(",") if p.strip()]
    if not parts:
        return "", ""
    return parts[0], " · ".join(parts[1:3])


def short_sub(sub, limit=96):
    import re

    sub = re.sub(r"\s*\([^)]*\)", "", sub or "").strip()
    first = re.split(r"(?<=[.!?])\s", sub)[0] if sub else ""
    if len(first) > limit:
        first = first[: limit - 1].rsplit(" ", 1)[0] + "…"
    return first


def shot_map(route=False):
    if not route and CONCEPT in ("bakir", "atlas") and P.get("tour", True):
        d = P.get("terrain") or ""
        mp = os.path.join(d, "meta.json")
        if d and os.path.isfile(mp):
            mk = json.load(open(mp, encoding="utf-8")).get("markers") or []
            beats = P.get("beats") or []
            if beats or (len(mk) >= 2 and FRAMES / FPS >= 4.0):
                if not beats:
                    beats = [{"name": mk[0].get("name", ""), "t": 0.0}]
                shot_journey(mk, beats)
                return
    hts, land, meta = load_terrain()
    build_terrain(hts, land, meta)
    lights()
    markers = meta.get("markers") or [{"name": P.get("title", ""), "sub": P.get("subtitle", ""), "x": 0.5, "y": 0.5}]
    m0 = markers[0]
    mx, my = to_world(m0["x"], m0["y"])
    mz = height_at(m0["x"], m0["y"])
    others = [m for m in markers[1:] if 0.02 < m.get("x", -1) < 0.98 and 0.02 < m.get("y", -1) < 0.98]
    pts_w = [(mx, my)] + [to_world(m["x"], m["y"]) for m in others]
    spread = max([math.hypot(px - mx, py - my) for px, py in pts_w] + [0.0])
    cx = sum(p[0] for p in pts_w) / len(pts_w)
    cy = sum(p[1] for p in pts_w) / len(pts_w)
    tilt = 55 if CONCEPT != "diorama" else 40
    if route and len(meta.get("route") or []) >= 2:
        pts = []
        for x, y in meta["route"]:
            wx, wy = to_world(x, y)
            pts.append((wx, wy, height_at(x, y)))
        route_curve(pts, int(FRAMES * 0.15), int(FRAMES * 0.75))
        rx = sum(p[0] for p in pts) / len(pts)
        ry = sum(p[1] for p in pts) / len(pts)
        span = max(math.hypot(p[0] - rx, p[1] - ry) for p in pts)
        cam = camera((rx, ry), max(10.0, span * 2.6), max(8.0, span * 2.1), 52)
    elif others:
        # overview of every place first, then a slow push into the main one
        cam = camera((mx, my), min(26.0, max(12.0, spread * 2.6)), 6.5, tilt, target_from=(cx, cy))
    else:
        cam = camera((mx, my), 12.0, 6.5, tilt)
    p = pin(mx, my, mz)
    p.scale = (0.8, 0.8, 0.8)
    fade(p, int(FRAMES * 0.12), int(FRAMES * 0.24))
    glow = 2.0 if CONCEPT == "bakir" else 0.0
    for n, m in enumerate(others[:6]):
        ox, oy = to_world(m["x"], m["y"])
        oz = height_at(m["x"], m["y"])
        q = pin(ox, oy, oz)
        q.scale = (0.55, 0.55, 0.55)
        f0 = int(FRAMES * (0.06 + 0.05 * n))
        fade(q, f0, f0 + 8)
        nm, rest = short_place(m.get("name", ""))
        if len(nm) > 16 and rest:
            nm = rest.split(" · ")[0]
        if nm:
            lab = text_obj(tr_upper(nm) if CONCEPT != "minyatur" else nm, 0.17, (ox + 0.14, oy, oz + 0.32), PAL["text"], bold=True, emission=glow * 0.6, name="Lbl")
            lab.rotation_euler = (math.radians(tilt), 0, 0)
            if hasattr(lab, "visible_shadow"):
                lab.visible_shadow = False
            fade(lab, f0 + 4, f0 + 14)
    name, place = short_place(m0.get("name") or P.get("title", ""))
    if CONCEPT != "minyatur":
        name, place = tr_upper(name), tr_upper(place)
    lower_third(cam, place, name, short_sub(m0.get("sub") or P.get("subtitle", "")), int(FRAMES * 0.32))


# ------------------------------------------------------------------ journey (animated map tour)
def tr_lower(s):
    return s.replace("İ", "i").replace("I", "ı").lower()


def tr_title(s):
    small = {"ve", "ile", "de", "da", "ya", "veya", "bir"}
    out = []
    for i, w in enumerate(tr_lower(s).split(" ")):
        if (i > 0 and w in small) or not w:
            out.append(w)
        else:
            # Er-Rakîm → Er-Rakîm, but the Turkish suffix in Ashâb-ı stays lower case
            out.append("-".join(tr_upper(p[:1]) + p[1:] if len(p) > 1 else p for p in w.split("-")))
    return " ".join(out)


def place_parts(name):
    """('YEDİ UYURLAR MAĞARASI, PANAYIRDAĞI, EFES/SELÇUK, İZMİR') -> ('EFES', 'Yedi Uyurlar Mağarası');
    ('Afşin (Arabissos/Efsûs), Kahramanmaraş') -> ('AFŞİN', 'Arabissos/Efsûs')."""
    import re

    paren = re.findall(r"\(([^)]*)\)", name or "")
    clean = re.sub(r"\s*\([^)]*\)", "", name or "").strip()
    parts = [p.strip() for p in clean.split(",") if p.strip()]
    if not parts:
        return "", ""
    if len(parts) >= 3:
        city, what = parts[-2], parts[0]
    elif len(parts) == 2:
        city, what = parts[0], (paren[0] if paren else parts[1])
    else:
        city, what = parts[0], (paren[0] if paren else "")
    city = city.split("/")[0].strip()
    if what and what == what.upper():
        what = tr_title(what)
    return tr_upper(city), what


def smooth(x):
    x = min(1.0, max(0.0, x))
    return x * x * x * (x * (x * 6 - 15) + 10)


def from_world(wx, wy):
    return wx / SX + 0.5, 0.5 - wy / SY


def ground(wx, wy):
    x, y = from_world(wx, wy)
    return height_at(x, y)


def billboard(cam, loc, name):
    """An empty that always faces the camera (labels, medallions)."""
    e = bpy.data.objects.new(name, None)
    bpy.context.scene.collection.objects.link(e)
    e.location = loc
    c = e.constraints.new("COPY_ROTATION")
    c.target = cam
    return e


def pop(ob, f, scale=1.0, dur=10):
    """Scale in with a small overshoot (animated, not a plain fade)."""
    ob.scale = (0.001, 0.001, 0.001)
    ob.keyframe_insert("scale", frame=max(1, f))
    s = scale * 1.18
    ob.scale = (s, s, s)
    ob.keyframe_insert("scale", frame=f + int(dur * 0.65))
    ob.scale = (scale, scale, scale)
    ob.keyframe_insert("scale", frame=f + dur)


def star_mesh(r_out, r_in, n=8, depth=0.01, name="Star"):
    """Eight-pointed star (Seljuk motif) as a thin solid."""
    verts, faces = [], []
    for k in range(2 * n):
        r = r_out if k % 2 == 0 else r_in
        a = math.pi * k / n + math.pi / 2
        verts.append((r * math.cos(a), r * math.sin(a), 0.0))
    for k in range(2 * n):
        x, y, _ = verts[k]
        verts.append((x, y, -depth))
    verts += [(0, 0, 0), (0, 0, -depth)]
    c0, c1 = 4 * n, 4 * n + 1
    m = 2 * n
    for k in range(m):
        k2 = (k + 1) % m
        faces.append((c0, k, k2))
        faces.append((c1, m + k2, m + k))
        faces.append((k, m + k, m + k2, k2))
    me = bpy.data.meshes.new(name)
    me.from_pydata(verts, [], faces)
    me.update()
    return me


def medallion(cam, wx, wy, f, main=False, city="", what="", sub=""):
    """Marker: a gold eight-pointed star in a ring, floating on a thin stem over a pulsing ground
    ring, with its label (city in the channel font, what is there underneath)."""
    gz = ground(wx, wy)
    k = 1.0 if main else 0.72
    lift = 0.55 * k
    gold = material("Gold", (1.0, 0.62, 0.22), 0.25, 1.0, emission=PAL["line"], strength=1.6)
    hot = material("GoldHot", (1.0, 0.7, 0.3), 0.2, 1.0, emission=PAL["line"], strength=5.0)
    dark = material("Bronze", (0.06, 0.035, 0.02), 0.35, 1.0)
    # stem
    bpy.ops.mesh.primitive_cylinder_add(radius=0.006, depth=1.0, location=(wx, wy, gz))
    stem = bpy.context.active_object
    stem.data.transform(Matrix.Translation((0, 0, 0.5)))
    stem.data.materials.append(hot)
    stem.scale = (1, 1, 0.001)
    stem.keyframe_insert("scale", frame=max(1, f))
    stem.scale = (1, 1, lift)
    stem.keyframe_insert("scale", frame=f + 8)
    # ground pulse: two rings expanding and fading out
    for j in range(2 if main else 1):
        bpy.ops.mesh.primitive_torus_add(major_radius=0.1, minor_radius=0.006, location=(wx, wy, gz + 0.01))
        ring = bpy.context.active_object
        ring.data.materials.append(hot)
        f0 = f + 2 + j * 9
        ring.scale = (0.2, 0.2, 1)
        ring.keyframe_insert("scale", frame=f0)
        ring.scale = (4.5 * k, 4.5 * k, 1)
        ring.keyframe_insert("scale", frame=f0 + 26)
        ring.scale = (0.001, 0.001, 0.001)
        ring.keyframe_insert("scale", frame=f0 + 27)
        if j == 0:
            # a quiet resting ring that stays
            bpy.ops.mesh.primitive_torus_add(major_radius=0.13 * k, minor_radius=0.005, location=(wx, wy, gz + 0.01))
            rest = bpy.context.active_object
            rest.data.materials.append(gold)
            pop(rest, f0 + 20)
    # the medallion faces the camera
    head = billboard(cam, (wx, wy, gz + lift), "Medal")
    bpy.ops.mesh.primitive_torus_add(major_radius=0.12, minor_radius=0.014, rotation=(math.pi / 2, 0, 0))
    rim = bpy.context.active_object
    rim.data.materials.append(gold)
    rim.parent = head
    rim.rotation_euler = (0, 0, 0)
    bpy.ops.mesh.primitive_cylinder_add(radius=0.112, depth=0.01)
    disc = bpy.context.active_object
    disc.data.materials.append(dark)
    disc.parent = head
    disc.location = (0, 0, -0.004)
    st = bpy.data.objects.new("Star", star_mesh(0.085, 0.042))
    bpy.context.scene.collection.objects.link(st)
    st.data.materials.append(hot if main else gold)
    st.parent = head
    st.location = (0, 0, 0.006)
    # slow spin of the star
    st.rotation_euler = (0, 0, 0)
    st.keyframe_insert("rotation_euler", frame=1)
    st.rotation_euler = (0, 0, math.radians(-45 if main else 30))
    st.keyframe_insert("rotation_euler", frame=FRAMES)
    pop(head, f + 4, k)
    if city:
        lab = bpy.data.objects.new("Label", None)
        bpy.context.scene.collection.objects.link(lab)
        lab.parent = head
        lab.location = (0.2, 0.0, 0.0)
        t = text_obj(city, 0.17, (0, 0.0, 0), PAL["text"], bold=True, emission=2.4, name="City", display=True)
        t.parent = lab
        if what:
            w = text_obj(what, 0.062, (0.004, -0.09, 0), PAL["accent"], emission=1.6, name="What")
            w.parent = lab
        if sub and main:
            s2 = text_obj(sub, 0.045, (0.004, -0.16, 0), PAL["text"], emission=0.9, name="Sub", box=1.6)
            s2.parent = lab
        # a gold rule between the medallion and the name
        rm = bpy.data.meshes.new("Lead")
        rm.from_pydata([(-0.07, 0.025, 0), (-0.01, 0.025, 0), (-0.01, 0.031, 0), (-0.07, 0.031, 0)], [], [(0, 1, 2, 3)])
        lead = bpy.data.objects.new("Lead", rm)
        bpy.context.scene.collection.objects.link(lead)
        lead.data.materials.append(hot)
        lead.parent = lab
        pop(lab, f + 10, 1.0, 12)
        for o in [t] + list(lab.children):
            if hasattr(o, "visible_shadow"):
                o.visible_shadow = False
    return head


def footsteps(path, f_of_u, step=0.16, size=0.05):
    """'Adım' animation: gold footprints pop in pair by pair along the path, just behind the
    travelling spark."""
    me = bpy.data.meshes.new("Foot")
    verts = []
    n = 14
    for i in range(n):
        a = 2 * math.pi * i / n
        # a sole: wider at the toes
        r = 1.0 + 0.25 * math.sin(a)
        verts.append((0.42 * size * math.cos(a) * r, size * math.sin(a), 0.0))
    me.from_pydata(verts, [], [list(range(n))])
    me.update()
    mat = material("FootM", (1.0, 0.6, 0.2), 0.3, 1.0, emission=PAL["line"], strength=3.2)
    me.materials.append(mat)
    # arc length table
    pts = [path(i / 200) for i in range(201)]
    lens = [0.0]
    for p, q in zip(pts, pts[1:]):
        lens.append(lens[-1] + math.hypot(q[0] - p[0], q[1] - p[1]))
    total = lens[-1]
    count = max(2, int(total / step))
    for i in range(1, count):
        d = total * i / count
        j = next((k for k, L in enumerate(lens) if L >= d), 200)
        u = j / 200
        x, y = pts[j][0], pts[j][1]
        p2 = pts[min(200, j + 1)]
        p1 = pts[max(0, j - 1)]
        ang = math.atan2(p2[1] - p1[1], p2[0] - p1[0])
        side = 1 if i % 2 else -1
        ox, oy = -math.sin(ang) * size * 0.55 * side, math.cos(ang) * size * 0.55 * side
        ob = bpy.data.objects.new("Step", me)
        bpy.context.scene.collection.objects.link(ob)
        ob.location = (x + ox, y + oy, ground(x, y) + 0.03)
        ob.rotation_euler = (0, 0, ang - math.pi / 2)
        if hasattr(ob, "visible_shadow"):
            ob.visible_shadow = False
        pop(ob, f_of_u(u), 1.0, 6)
    return total


# ------------------------------------------------------------------ Kadim Atlas graphics
def soft_plate_material(name, color=(0.0, 0.0, 0.0), alpha=0.95, edge=0.3):
    """Dark backing with feathered edges (UV based), so lines on the map never cross type."""
    m = bpy.data.materials.new(name)
    m.use_nodes = True
    nt = m.node_tree
    for n_ in list(nt.nodes):
        nt.nodes.remove(n_)
    tc = nt.nodes.new("ShaderNodeTexCoord")
    sep = nt.nodes.new("ShaderNodeSeparateXYZ")
    nt.links.new(tc.outputs["UV"], sep.inputs[0])

    def edge_fac(sock):
        a = nt.nodes.new("ShaderNodeMath")
        a.operation = "SUBTRACT"
        a.inputs[0].default_value = 1.0
        nt.links.new(sock, a.inputs[1])
        mn = nt.nodes.new("ShaderNodeMath")
        mn.operation = "MINIMUM"
        nt.links.new(sock, mn.inputs[0])
        nt.links.new(a.outputs[0], mn.inputs[1])
        dv = nt.nodes.new("ShaderNodeMath")
        dv.operation = "DIVIDE"
        dv.use_clamp = True
        nt.links.new(mn.outputs[0], dv.inputs[0])
        dv.inputs[1].default_value = edge
        return dv.outputs[0]

    mul = nt.nodes.new("ShaderNodeMath")
    mul.operation = "MULTIPLY"
    nt.links.new(edge_fac(sep.outputs["X"]), mul.inputs[0])
    nt.links.new(edge_fac(sep.outputs["Y"]), mul.inputs[1])
    amt = nt.nodes.new("ShaderNodeMath")
    amt.operation = "MULTIPLY"
    nt.links.new(mul.outputs[0], amt.inputs[0])
    amt.inputs[1].default_value = alpha
    em = nt.nodes.new("ShaderNodeEmission")
    em.inputs["Color"].default_value = (*color, 1)
    tr = nt.nodes.new("ShaderNodeBsdfTransparent")
    mix = nt.nodes.new("ShaderNodeMixShader")
    nt.links.new(amt.outputs[0], mix.inputs[0])
    nt.links.new(tr.outputs[0], mix.inputs[1])
    nt.links.new(em.outputs[0], mix.inputs[2])
    out = nt.nodes.new("ShaderNodeOutputMaterial")
    nt.links.new(mix.outputs[0], out.inputs["Surface"])
    if hasattr(m, "surface_render_method"):
        m.surface_render_method = "DITHERED"  # reliable in EEVEE Next (blended can drop out)
    elif hasattr(m, "blend_method"):
        m.blend_method = "HASHED"
    return m


def plate(parent, x, y, w, h, alpha=0.6, coll=None, z=-0.002):
    me = bpy.data.meshes.new("Plate")
    me.from_pydata([(x, y, z), (x + w, y, z), (x + w, y + h, z), (x, y + h, z)], [], [(0, 1, 2, 3)])
    uv = me.uv_layers.new()
    for li, (u, v) in enumerate(((0, 0), (1, 0), (1, 1), (0, 1))):
        uv.data[li].uv = (u, v)
    ob = bpy.data.objects.new("Plate", me)
    (coll or bpy.context.scene.collection).objects.link(ob)
    ob.parent = parent
    ob.data.materials.append(soft_plate_material("PlateM", alpha=alpha))
    if hasattr(ob, "visible_shadow"):
        ob.visible_shadow = False
    return ob


def text_width(ob):
    bpy.context.view_layer.update()
    return ob.dimensions.x


def medallion_atlas(cam, wx, wy, f, number=None, city="", what="", main=True):
    """Kadim Atlas marker: dark disc, amber double ring with a star (or a count number), a tick
    pointing at the place, a thin stem to the ground and a pulse ring; label underneath on a plate."""
    gz = ground(wx, wy)
    k = 1.0 if main else 0.8
    lift = 0.42 * k
    amber = material("AmberE", PAL["line"], 0.4, 0.0, emission=PAL["line"], strength=1.0)
    amber_soft = material("AmberS", PAL["line"], 0.4, 0.0, emission=PAL["line"], strength=0.7)
    disc_m = material("Disc", (0.03, 0.025, 0.02), 0.5, 0.0, emission=(0.02, 0.016, 0.012), strength=1.0)
    cream = material("CreamE", PAL["text"], 0.5, 0.0, emission=PAL["text"], strength=1.0)
    bpy.ops.mesh.primitive_cylinder_add(radius=0.004, depth=1.0, location=(wx, wy, gz))
    stem = bpy.context.active_object
    stem.data.transform(Matrix.Translation((0, 0, 0.5)))
    stem.data.materials.append(amber_soft)
    stem.scale = (1, 1, 0.001)
    stem.keyframe_insert("scale", frame=max(1, f))
    stem.scale = (1, 1, lift - 0.13 * k)
    stem.keyframe_insert("scale", frame=f + 8)
    bpy.ops.mesh.primitive_torus_add(major_radius=0.1, minor_radius=0.005, location=(wx, wy, gz + 0.01))
    ring = bpy.context.active_object
    ring.data.materials.append(amber)
    ring.scale = (0.2, 0.2, 1)
    ring.keyframe_insert("scale", frame=f + 2)
    ring.scale = (1.9 * k, 1.9 * k, 1)
    ring.keyframe_insert("scale", frame=f + 22)
    ring.scale = (0.001, 0.001, 0.001)
    ring.keyframe_insert("scale", frame=f + 27)
    head = billboard(cam, (wx, wy, gz + lift), "Medal")
    r = 0.12
    bpy.ops.mesh.primitive_cylinder_add(radius=r, depth=0.004)
    disc = bpy.context.active_object
    disc.rotation_euler = (0, 0, 0)
    disc.data.materials.append(disc_m)
    disc.parent = head
    for rr, th in ((r, 0.011), (r * 0.8, 0.004)):
        bpy.ops.mesh.primitive_torus_add(major_radius=rr, minor_radius=th, major_segments=64)
        t = bpy.context.active_object
        t.data.materials.append(amber)
        t.parent = head
        t.location = (0, 0, 0.004)
    # tick under the disc, pointing at the place
    tm = bpy.data.meshes.new("Tick")
    tm.from_pydata([(-0.03, -r * 0.98, 0.003), (0.03, -r * 0.98, 0.003), (0, -r * 1.42, 0.003)], [], [(0, 1, 2)])
    tick = bpy.data.objects.new("Tick", tm)
    bpy.context.scene.collection.objects.link(tick)
    tick.data.materials.append(amber)
    tick.parent = head
    if number is None:
        st = bpy.data.objects.new("Star", star_mesh(r * 0.55, r * 0.27, depth=0.003))
        bpy.context.scene.collection.objects.link(st)
        st.data.materials.append(amber)
        st.parent = head
        st.location = (0, 0, 0.006)
    else:
        nt_ = text_obj(str(number), r * 1.25, (0, 0, 0.006), PAL["text"], bold=True, align="CENTER", name="Num")
        nt_.data.align_y = "CENTER"
        nt_.data.materials.clear()
        nt_.data.materials.append(cream)
        nt_.parent = head
    pop(head, f + 4, k)
    for o in [stem, ring] + list(head.children):
        if hasattr(o, "visible_shadow"):
            o.visible_shadow = False
    if city:
        lab = bpy.data.objects.new("Label", None)
        bpy.context.scene.collection.objects.link(lab)
        lab.parent = head
        # a little toward the camera, so the stem passes behind the label plate, never through letters
        lab.location = (0, -r * 1.75, 0.2)
        t = text_obj(city, 0.13, (0, -0.13, 0.004), PAL["text"], align="CENTER", name="City", display=True)
        t.data.materials.clear()
        t.data.materials.append(cream)
        t.parent = lab
        w_ = text_width(t)
        h_ = 0.2
        if what:
            wt = text_obj(what, 0.062, (0, -0.215, 0.004), PAL["accent"], bold=True, align="CENTER", name="What")
            wt.data.materials.clear()
            wt.data.materials.append(material("AmberT", PAL["line"], 0.4, 0.0, emission=PAL["line"], strength=1.0))
            wt.parent = lab
            w_ = max(w_, text_width(wt))
            h_ = 0.29
        plate(lab, -w_ / 2 - 0.14, -h_ - 0.09, w_ + 0.28, h_ + 0.14, alpha=0.92)
        for o in lab.children:
            if hasattr(o, "visible_shadow"):
                o.visible_shadow = False
        pop(lab, f + 10, 1.0, 12)
    return head


def dashes(path, f_of_u, L):
    """Amber dashed route that draws itself, ending in an arrow head."""
    dash = max(0.07, min(0.18, L / 30))
    gap = dash * 0.62
    width = max(0.018, min(0.032, L / 260))
    me = bpy.data.meshes.new("Dash")
    me.from_pydata([(-dash / 2, -width / 2, 0), (dash / 2, -width / 2, 0), (dash / 2, width / 2, 0), (-dash / 2, width / 2, 0)], [], [(0, 1, 2, 3)])
    me.materials.append(material("DashM", PAL["line"], 0.4, 0.0, emission=PAL["line"], strength=1.1))
    pts = [path(i / 400) for i in range(401)]
    lens = [0.0]
    for p, q in zip(pts, pts[1:]):
        lens.append(lens[-1] + math.hypot(q[0] - p[0], q[1] - p[1]))
    total = lens[-1]
    start, end = 0.16, total - 0.24  # leave the medallions clear
    d = start
    while d < end:
        j = next((k for k, Lk in enumerate(lens) if Lk >= d + dash / 2), 400)
        x, y = pts[j]
        p2, p1 = pts[min(400, j + 1)], pts[max(0, j - 1)]
        ang = math.atan2(p2[1] - p1[1], p2[0] - p1[0])
        ob = bpy.data.objects.new("D", me)
        bpy.context.scene.collection.objects.link(ob)
        ob.location = (x, y, ground(x, y) + 0.035)
        ob.rotation_euler = (0, 0, ang)
        if hasattr(ob, "visible_shadow"):
            ob.visible_shadow = False
        pop(ob, f_of_u(j / 400), 1.0, 4)
        d += dash + gap
    # arrow head
    j = next((k for k, Lk in enumerate(lens) if Lk >= end), 400)
    x, y = pts[j]
    p2, p1 = pts[min(400, j + 2)], pts[max(0, j - 2)]
    ang = math.atan2(p2[1] - p1[1], p2[0] - p1[0])
    a_ = width * 3.2
    am = bpy.data.meshes.new("Arrow")
    am.from_pydata([(a_ * 1.5, 0, 0), (-a_ * 0.6, a_, 0), (-a_ * 0.6, -a_, 0)], [], [(0, 1, 2)])
    am.materials.append(material("ArrowM", PAL["line"], 0.4, 0.0, emission=PAL["line"], strength=1.1))
    arrow = bpy.data.objects.new("Arrow", am)
    bpy.context.scene.collection.objects.link(arrow)
    arrow.location = (x, y, ground(x, y) + 0.04)
    arrow.rotation_euler = (0, 0, ang)
    pop(arrow, f_of_u(j / 400), 1.0, 5)


def atlas_caption(kicker, title, body, highlight, f0, top=False, scale=1.0):
    """Kadim Atlas caption in the flat overlay: amber kicker tag, channel-font title, body with the
    highlight phrase in amber, all on a soft dark plate; plus the map source line."""
    global LINK_TO
    ov, _ = overlay_scene(bpy.context.scene)
    LINK_TO = ov.collection
    fw, fh = (1.0, 1.0 / ASPECT) if ASPECT >= 1 else (ASPECT, 1.0)
    rig = bpy.data.objects.new("AtlasCaption", None)
    ov.collection.objects.link(rig)
    x0 = -fw / 2 + fw * 0.05
    size = fw * (0.052 if ASPECT >= 1 else 0.085) * scale
    ink = PAL["text"]
    amber = PAL["accent"]
    lines_h = 0.0
    objs = []
    # body first (it sits under the title); wrap inside 55% of the frame
    body_ob = None
    if body:
        body_ob = text_obj(body, size * 0.4, (0, 0, 0), ink, bold=False, name="Body", box=fw * (0.55 if ASPECT >= 1 else 0.86))
        body_ob.data.materials.clear()
        body_ob.data.materials.append(flat("BodyF", ink))
        body_ob.data.materials.append(flat("BodyA", amber))
        if highlight:
            lo = fold_tr(body)
            hi = fold_tr(highlight)
            at = lo.find(hi)
            if at >= 0:
                for i in range(at, at + len(highlight)):
                    if i < len(body_ob.data.body_format):
                        body_ob.data.body_format[i].material_index = 1
        body_ob.parent = rig
        bpy.context.view_layer.update()
    title_ob = text_obj(title, size, (0, 0, 0), ink, name="Title", display=True)
    title_ob.data.materials.clear()
    title_ob.data.materials.append(flat("TitleF", ink))
    title_ob.parent = rig
    bpy.context.view_layer.update()
    tw = title_ob.dimensions.x
    th = size * 0.95
    bh = body_ob.dimensions.y if body_ob else 0.0
    kick_h = size * 0.5 if kicker else 0.0
    # vertical stack (y grows upward): body, title, kicker
    y_body = 0.0
    y_title = (bh + size * 0.35) if body_ob else 0.0
    y_kick = y_title + th + size * 0.22
    if body_ob:
        body_ob.location = (0, bh - size * 0.4, 0)
    title_ob.location = (0, y_title, 0)
    total_h = y_kick + kick_h
    if kicker:
        kf = size * 0.36
        k_ob = text_obj(kicker, kf, (0, 0, 0.001), (0, 0, 0), bold=True, name="Kicker")
        k_ob.data.materials.clear()
        k_ob.data.materials.append(flat("KickF", srgb((0.06, 0.05, 0.04))))
        k_ob.parent = rig
        bpy.context.view_layer.update()
        kw = k_ob.dimensions.x
        pad = kf * 0.45
        hgt = kf * 1.45
        sk = hgt * 0.35
        pm = bpy.data.meshes.new("Pill")
        pm.from_pydata([(-pad, -kf * 0.32, 0), (kw + pad, -kf * 0.32, 0), (kw + pad + sk, hgt - kf * 0.32, 0), (-pad + sk, hgt - kf * 0.32, 0)], [], [(0, 1, 2, 3)])
        pill = bpy.data.objects.new("Pill", pm)
        ov.collection.objects.link(pill)
        pill.parent = rig
        pill.location = (0, y_kick, 0)
        pill.data.materials.append(flat("PillF", amber))
        k_ob.location = (sk * 0.5, y_kick, 0.001)
        objs.append((pill, f0))
        objs.append((k_ob, f0))
    width = max(tw, body_ob.dimensions.x if body_ob else 0.0) + size * 0.8
    plate(rig, -size * 0.7, -size * 0.8, width + size * 0.8, total_h + size * 1.5, alpha=0.9, coll=ov.collection, z=-0.01)
    if top:
        rig.location = (x0, fh / 2 - fh * 0.08 - total_h, 0)
    else:
        rig.location = (x0, -fh / 2 + fh * 0.16, 0)
    objs.append((title_ob, f0 + 3))
    if body_ob:
        objs.append((body_ob, f0 + 10))
    for ob, fi in objs:
        fade(ob, fi, fi + 9)
    # source line (bottom-left, small)
    src = P.get("credit") or ""
    if src:
        c = text_obj(src, fw * 0.0095, (-fw / 2 + fw * 0.02, -fh / 2 + fh * 0.03, 0), ink, name="Credit")
        c.data.materials.clear()
        c.data.materials.append(flat("CreditF", srgb((0.62, 0.58, 0.52))))
    LINK_TO = None
    return rig


def fold_tr(s):
    return s.replace("İ", "i").replace("I", "ı").lower()


def shot_journey(markers, beats):
    """The map follows the narration. `beats` (from CrStudio) say which place is spoken of when:
    [{"name": marker name, "t": seconds, "origin": bool}]. The camera opens on the region, arrives
    at each place just as the narrator names it, and travels between places step by step (gold
    footprints, a spark leading the way), lifting off for the hop and diving in again. A single
    place gets a slow descent and a closing push-in instead."""
    sc = bpy.context.scene
    hts, land, meta = load_terrain()
    build_terrain(hts, land, meta)
    lights()
    for ob in list(sc.objects):
        if ob.type == "LIGHT" and ob.data.type != "SUN" and not ob.constraints:
            aim = ob.constraints.new("TRACK_TO")
            aim.target = bpy.data.objects.get("Terrain")
    fps = FPS
    S = FRAMES / fps
    by_name = {tr_lower(m.get("name", "")).replace("â", "a").replace("î", "i").replace("û", "u"): m for m in markers}
    stops = []
    for b in sorted(beats, key=lambda b: float(b.get("t", 0))):
        m = by_name.get(tr_lower(b.get("name", "")).replace("â", "a").replace("î", "i").replace("û", "u"))
        if not m or not (0.0 <= m.get("x", -1) <= 1.0 and 0.0 <= m.get("y", -1) <= 1.0):
            continue
        if any(st[1] is m for st in stops):
            continue
        stops.append((to_world(m["x"], m["y"]), m, float(b.get("t", 0)), bool(b.get("origin"))))
    if not stops:
        m = markers[0]
        stops = [(to_world(m["x"], m["y"]), m, 0.0, False)]
    n = len(stops)
    # arrival times: just before the name is spoken; the first one after a short establishing move
    arr = []
    for i, (_, _, t, _) in enumerate(stops):
        lo = 1.3 if i == 0 else arr[-1] + 1.3
        arr.append(min(max(lo, t - 0.3), S - 0.8))
    # each leg leaves as late as it can (the narrator is still talking about the last place)
    leaves = []
    for i in range(n - 1):
        gap = arr[i + 1] - arr[i]
        dur = min(3.4, max(1.0, gap * 0.72))
        leaves.append(max(arr[i] + 0.35, arr[i + 1] - dur))
    xs = [p[0][0] for p in stops]
    ys = [p[0][1] for p in stops]
    ccx, ccy = sum(xs) / n, sum(ys) / n
    spread = max([math.hypot(x - ccx, y - ccy) for x, y in zip(xs, ys)] + [0.0])
    d_near = 3.4 if n > 1 else 2.6
    d_far = min(22.0, max(7.0, spread * 2.6 + 4.0)) if n > 1 else 7.5
    tilt_far, tilt_near = (62.0, 46.0) if n > 1 else (58.0, 40.0)

    def leg_path(i):
        (ax, ay), (bx, by) = stops[i][0], stops[i + 1][0]
        L = math.hypot(bx - ax, by - ay) or 1.0
        nx, ny = -(by - ay) / L, (bx - ax) / L
        bend = 0.2 * L * (1 if i % 2 == 0 else -1)
        mx, my = (ax + bx) / 2 + nx * bend, (ay + by) / 2 + ny * bend

        def p(u):
            v = 1 - u
            return (v * v * ax + 2 * v * u * mx + u * u * bx, v * v * ay + 2 * v * u * my + u * u * by)

        return p, L

    paths = [leg_path(i) for i in range(n - 1)]

    def state(tt):
        # a slow, constant drift under every move (orbit + push) so the picture never stands still:
        # eased stops read as a frozen frame on screen
        (tx, ty), dist, tilt, yaw = _state(tt)
        k = tt / max(0.01, S)
        return (tx, ty), dist * (1.0 - 0.07 * k), tilt, yaw - 7.0 + 14.0 * k

    def _state(tt):
        if tt <= arr[0]:
            e = smooth(tt / max(0.01, arr[0]))
            sx, sy = stops[0][0]
            ox, oy = (ccx, ccy) if n > 1 else (sx, sy)
            return (ox + (sx - ox) * e, oy + (sy - oy) * e), d_far + (d_near - d_far) * e, tilt_far + (tilt_near - tilt_far) * e, -10 + 10 * e
        for i in range(n - 1):
            if tt <= leaves[i]:
                u = (tt - arr[i]) / max(0.01, leaves[i] - arr[i])
                return stops[i][0], d_near - 0.25 * smooth(u), tilt_near, 4 * smooth(u) * (1 if i % 2 == 0 else -1)
            if tt <= arr[i + 1]:
                u = smooth((tt - leaves[i]) / max(0.01, arr[i + 1] - leaves[i]))
                p, L = paths[i]
                hop = math.sin(math.pi * u)
                sgn = 1 if i % 2 == 0 else -1
                return p(u), d_near - 0.25 + 0.25 * u + max(2.0, L * 1.1) * hop, tilt_near + 14 * hop, sgn * 4 * (1 - u)
        # last place: a slow closing push-in (the 'zoom' beat)
        # ease in, but keep moving into the cut (no ease-out)
        x = min(1.0, max(0.0, (tt - arr[-1]) / max(0.01, S - arr[-1])))
        u = 0.5 * (smooth(x) + x)
        return stops[-1][0], d_near - (0.9 if n == 1 else 0.6) * u, tilt_near - 4 * u, 5 * u

    cam_d = bpy.data.cameras.new("Cam")
    cam_d.lens = 40
    cam_d.clip_end = 400
    cam = bpy.data.objects.new("Cam", cam_d)
    sc.collection.objects.link(cam)
    sc.camera = cam
    tgt = bpy.data.objects.new("Target", None)
    sc.collection.objects.link(tgt)
    tr = cam.constraints.new("TRACK_TO")
    tr.target = tgt
    for f in range(1, FRAMES + 1):
        (tx, ty), dist, tilt, yaw = state((f - 1) / fps)
        # aim between the ground and the medallion so the closing push-in keeps the marker in frame
        tz = ground(tx, ty) * 0.6 + 0.16
        a, tl = math.radians(yaw), math.radians(tilt)
        if CONCEPT == "atlas":
            # the place sits right of centre: the caption owns the left side of the frame
            off = 0.17 * dist
            tx, ty = tx - off * math.cos(a), ty - off * math.sin(a)
        tgt.location = (tx, ty, tz)
        tgt.keyframe_insert("location", frame=f)
        cam.location = (tx + dist * math.sin(a) * math.cos(tl), ty - dist * math.cos(a) * math.cos(tl), tz + dist * math.sin(tl))
        cam.keyframe_insert("location", frame=f)

    counting = n >= 3 and not any(st[3] for st in stops)
    for i, ((wx, wy), m, _, origin) in enumerate(stops):
        city, what = place_parts(m.get("name", ""))
        sub = short_sub(m.get("sub", ""), 70) if (n == 1 and not origin) else ""
        f_in = max(2, int(fps * arr[i]) - 6)
        if CONCEPT == "atlas":
            hd = medallion_atlas(cam, wx, wy, f_in, number=(i + 1) if counting else None, city=city, what=what, main=not origin)
            if i < n - 1:
                # once the story moves on, the place steps back (smaller) so it never fills the foreground
                f_q = int(fps * (leaves[i] + 0.6))
                s0 = 1.0 if not origin else 0.8
                hd.scale = (s0, s0, s0)
                hd.keyframe_insert("scale", frame=max(f_in + 16, f_q))
                hd.scale = (0.5 * s0, 0.5 * s0, 0.5 * s0)
                hd.keyframe_insert("scale", frame=max(f_in + 28, f_q + 14))
        else:
            medallion(cam, wx, wy, f_in, main=not origin, city=city, what=what, sub=sub)
    for i in range(n - 1):
        p, L = paths[i]
        f0, f1 = leaves[i] * fps, arr[i + 1] * fps

        def f_of_u(u, f0=f0, f1=f1):
            lo, hi = 0.0, 1.0
            for _ in range(30):
                mid = (lo + hi) / 2
                if smooth(mid) < u:
                    lo = mid
                else:
                    hi = mid
            return int(f0 + (f1 - f0) * lo)

        if CONCEPT == "atlas":
            dashes(p, f_of_u, L)
        else:
            footsteps(p, f_of_u, step=max(0.12, min(0.3, L / 26)), size=max(0.035, min(0.07, L / 60)))
        bpy.ops.mesh.primitive_uv_sphere_add(radius=0.035, segments=16, ring_count=8)
        spark = bpy.context.active_object
        spark.data.materials.append(material("Spark", (1, 0.8, 0.5), 0.2, 0.0, emission=(1.0, 0.75, 0.4), strength=14.0))
        for f in range(int(f0), int(f1) + 2):
            u = smooth((f - f0) / max(1.0, f1 - f0))
            x, y = p(u)
            spark.location = (x, y, ground(x, y) + 0.06)
            spark.keyframe_insert("location", frame=max(1, f))
        spark.scale = (0.001,) * 3
        spark.keyframe_insert("scale", frame=max(1, int(f0) - 1))
        spark.scale = (1,) * 3
        spark.keyframe_insert("scale", frame=int(f0) + 2)
        spark.keyframe_insert("scale", frame=int(f1) - 1)
        spark.scale = (0.001,) * 3
        spark.keyframe_insert("scale", frame=int(f1) + 3)
    if CONCEPT == "atlas":
        # type goes where the places are not (judged on the last frame)
        sc.frame_set(FRAMES)
        bpy.context.view_layer.update()
        from bpy_extras.object_utils import world_to_camera_view
        from mathutils import Vector

        views = [world_to_camera_view(sc, cam, Vector((x, y, ground(x, y)))) for (x, y), *_ in stops]
        sc.frame_set(1)
        ys = [1 - v.y for v in views]
        # places on the right half leave the lower left free; otherwise the side away from them
        top = False if all(v.x > 0.55 for v in views) else (sum(ys) / len(ys)) > 0.5
        atlas_caption(P.get("kicker", ""), P.get("caption_title", ""), P.get("body", ""), P.get("highlight", ""), max(2, int(fps * 0.4)), top=top)
    log("journey", [(place_parts(st[1].get("name", ""))[0], round(a, 2)) for st, a in zip(stops, arr)], "S", round(S, 1))


def shot_title_atlas(kind):
    """Kadim Atlas chapter / intro card: a slow aerial drift over the story's real land (darkened),
    the caption large in the lower left: tag (chapter no. / channel), title, one line."""
    sc = bpy.context.scene
    hts, land, meta = load_terrain()
    build_terrain(hts, land, meta)
    lights()
    cam_d = bpy.data.cameras.new("Cam")
    cam_d.lens = 38
    cam_d.clip_end = 400
    cam = bpy.data.objects.new("Cam", cam_d)
    sc.collection.objects.link(cam)
    sc.camera = cam
    tgt = bpy.data.objects.new("Target", None)
    sc.collection.objects.link(tgt)
    tr = cam.constraints.new("TRACK_TO")
    tr.target = tgt
    mk = (meta.get("markers") or [{"x": 0.5, "y": 0.5}])[0]
    cx, cy = to_world(mk.get("x", 0.5), mk.get("y", 0.5))
    for f, dist, yaw, tilt, dx in ((1, 7.5, -14.0, 38.0, -0.6), (FRAMES, 6.2, 6.0, 33.0, 0.6)):
        a, t = math.radians(yaw), math.radians(tilt)
        tgt.location = (cx + dx, cy, 0.15)
        tgt.keyframe_insert("location", frame=f)
        cam.location = (cx + dx + dist * math.sin(a) * math.cos(t), cy - dist * math.cos(a) * math.cos(t), dist * math.sin(t))
        cam.keyframe_insert("location", frame=f)
    # constant-speed aerial drift: an eased card stands still at both ends
    _ease(cam, "LINEAR")
    _ease(tgt, "LINEAR")
    # darken the land under the type: the card is about the words
    sun = bpy.data.objects.get("Sun")
    if sun:
        sun.data.energy = 2.2
    big = 1.55 if kind == "intro" else 1.35
    atlas_caption(P.get("kicker", ""), P.get("title", ""), P.get("subtitle", ""), P.get("highlight", ""), max(2, int(FPS * 0.35)), top=False, scale=big)


def shot_title(kind):
    """Intro / chapter: the title rises over a slowly drifting terrain."""
    if CONCEPT == "atlas":
        shot_title_atlas(kind)
        return
    hts, land, meta = load_terrain()
    build_terrain(hts, land, meta)
    lights()
    camera((0, 0), 14, 11, 30)
    cam = bpy.context.scene.camera
    kicker = P.get("kicker", "")
    title = P.get("title", "")
    # texts face the camera
    holder = bpy.data.objects.new("TitleRig", None)
    bpy.context.scene.collection.objects.link(holder)
    holder.parent = cam
    holder.location = (0, 0, -6)
    big = 0.62 if kind == "intro" else 0.5
    t = text_obj(tr_upper(title) if CONCEPT != "minyatur" else title, big, (0, -0.15, 0), PAL["text"], bold=True, display=True, align="CENTER", emission=3.0 if CONCEPT == "bakir" else 0.0, extrude=0.02)
    t.parent = holder
    fade(t, int(FRAMES * 0.12), int(FRAMES * 0.32))
    if kicker:
        k = text_obj(tr_upper(kicker), 0.2, (0, 0.62, 0), PAL["accent"], bold=True, align="CENTER", emission=2.0 if CONCEPT == "bakir" else 0.0)
        k.parent = holder
        fade(k, int(FRAMES * 0.05), int(FRAMES * 0.2))
    sub = P.get("subtitle", "")
    if sub:
        s = text_obj(sub, 0.2, (0, -0.65, 0), PAL["text"], align="CENTER", emission=1.0 if CONCEPT == "bakir" else 0.0)
        s.parent = holder
        fade(s, int(FRAMES * 0.3), int(FRAMES * 0.48))
    # a thin rule under the title
    bpy.ops.mesh.primitive_plane_add(size=1)
    rule = bpy.context.active_object
    rule.parent = holder
    rule.location = (0, -0.42, 0)
    rule.scale = (2.2, 0.012, 1)
    rule.data.materials.append(material("Rule", PAL["accent"], 0.4, 0.5, emission=PAL["accent"], strength=3.0 if CONCEPT == "bakir" else 0.0))
    fade(rule, int(FRAMES * 0.2), int(FRAMES * 0.38))


def output(sc):
    out = P.get("out") or "/tmp/crstudio_shot.mov"
    os.makedirs(os.path.dirname(out) or ".", exist_ok=True)
    r = sc.render
    r.image_settings.file_format = "FFMPEG"
    ff = r.ffmpeg
    try:
        ff.format = "QUICKTIME"
        ff.codec = "PRORES"
        if hasattr(ff, "ffmpeg_prores_profile"):
            ff.ffmpeg_prores_profile = "422_HQ"
        r.filepath = out
        log("output ProRes", out)
    except Exception as e:
        log("ProRes unavailable, H.264:", e)
        ff.format = "MPEG4"
        ff.codec = "H264"
        ff.constant_rate_factor = "HIGH"
        r.filepath = os.path.splitext(out)[0] + ".mp4"
    ff.audio_codec = "NONE"
    r.use_file_extension = False


def main():
    sc = reset_scene()
    if SHOT in ("map", "route"):
        shot_map(route=SHOT == "route")
    else:
        shot_title(SHOT)
    output(sc)
    log("render", CONCEPT, SHOT, W, H, FRAMES)
    if P.get("still"):
        sc.frame_set(max(1, int(FRAMES * float(P.get("still_at", 0.8)))))
        sc.render.image_settings.file_format = "PNG"
        sc.render.filepath = P["still"]
        bpy.ops.render.render(write_still=True)
    else:
        bpy.ops.render.render(animation=True)
    log("done", sc.render.filepath)


if __name__ == "__main__":
    main()
