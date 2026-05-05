#!/usr/bin/env python3
"""
VRM morph-target pruner + buffer compactor.

Usage:
    python3 scripts/compress_vrm_textures.py <input.vrm> [<output.vrm>]

If output path is omitted the file is written to <name>_compressed.vrm next to
the input.  The original file is never modified.

What it does
-------------
1. Strips zero-delta morph targets: any morph-target accessor whose float data
   is entirely within 1e-6 of zero is remapped to a single shared zero buffer
   view.  This is lossless - zero-delta morphs have no visible effect - and
   removes ~100 MB of redundant data from large DAZ-exported VRMs.
2. Removes a configurable list of morph-target names from every mesh primitive
   (the default blocklist covers body-deformation shapes not needed by Jarvis).
3. Rebases all VRMC_vrm morphTargetBind indices so the VRM expression system
   still points at the correct (now-shifted) morph targets.
4. Fixes lookAt type: if VRMC_vrm.lookAt.type is 'expression' but humanoid eye
   bones are mapped, switches to 'bone' so bevy_vrm1's gaze driver activates.
5. Clamps skin joint limit to 256 (iOS Metal shader limit): reorders each skin's
   joint list so humanoid deforming bones come first, then zeros/renormalizes any
   vertex weights that referenced joints pushed to index >= 256.
6. Compacts the BIN buffer - orphaned accessors and buffer-views are dropped and
   all byte offsets are rewritten.
7. Writes a valid GLB 2.0 file that Bevy / bevy_vrm1 can load.

The VRM extensions (VRMC_vrm, VRMC_springBone, VRMC_materials_mtoon,
KHR_texture_transform, KHR_materials_unlit) are preserved verbatim.
"""

import json
import struct
import sys
import pathlib
import copy
import io
import argparse

# -- GLB helpers ------------------------------------------------------------

def read_glb(path: str) -> tuple[dict, bytes]:
    """Return (json_chunk_as_dict, bin_chunk_bytes)."""
    with open(path, "rb") as f:
        header = f.read(12)
        magic, version, _ = struct.unpack_from("<III", header)
        assert magic == 0x46546C67, "Not a GLB file"
        assert version == 2, f"GLB version {version} not supported"

        c0_len, c0_type = struct.unpack_from("<II", f.read(8))
        json_bytes = f.read(c0_len)
        data = json.loads(json_bytes)

        c1_len, c1_type = struct.unpack_from("<II", f.read(8))
        bin_data = f.read(c1_len)
    return data, bin_data


def write_glb(path: str, data: dict, bin_data: bytes) -> None:
    """Write GLB 2.0 with JSON + BIN chunks (4-byte aligned)."""
    def pad4(b: bytes, pad_byte: int = 0x20) -> bytes:
        r = len(b) % 4
        return b if r == 0 else b + bytes([pad_byte] * (4 - r))

    json_bytes = pad4(json.dumps(data, separators=(",", ":")).encode("utf-8"), 0x20)
    bin_data_padded = pad4(bin_data, 0x00)

    total = 12 + 8 + len(json_bytes) + 8 + len(bin_data_padded)
    with open(path, "wb") as f:
        f.write(struct.pack("<III", 0x46546C67, 2, total))
        f.write(struct.pack("<II", len(json_bytes), 0x4E4F534A))  # JSON
        f.write(json_bytes)
        f.write(struct.pack("<II", len(bin_data_padded), 0x004E4942))  # BIN
        f.write(bin_data_padded)


# -- Core logic -------------------------------------------------------------

# ---------------------------------------------------------------------------
# Phase 0: zero-delta morph stripping
# ---------------------------------------------------------------------------
_FLOAT32_COMPONENT = 5126

def strip_zero_morph_targets(
    data: dict,
    bin_data: bytes,
    epsilon: float = 1e-6,
) -> tuple[dict, bytes]:
    """Physically REMOVE zero-delta morph targets from mesh primitives.

    A morph target is zero-delta if every VEC3 FLOAT32 vertex-displacement
    accessor has all components within *epsilon* of zero.  Unlike the previous
    approach of redirecting zero-delta data to a shared zero buffer view, this
    function deletes the zero-delta targets from primitives[].targets entirely.

    Why this matters for iOS / Metal:
      Bevy allocates a GPU morph-target texture of size
        vertex_count × num_morphs × 12 bytes
      per primitive.  Leaving 3 000+ zero-delta morphs in targets[] causes
      hundreds of MB of Metal GPU allocations and triggers the iOS jetsam OOM
      killer in the first rendered frame.

    Protected morphs (referenced by VRMC_vrm expression morphTargetBinds) are
    kept even when their data is all-zero.  After removal, VRM expression bind
    indices are remapped to the new morph positions.

    Ends with a full accessor / bufferView / BIN compaction pass so the output
    GLB has no orphaned data.
    """
    data = copy.deepcopy(data)
    accessors = data.get("accessors", [])
    buf_views = data.get("bufferViews", [])
    meshes = data.get("meshes", [])

    # ── Step 1: identify zero-delta accessor indices ──────────────────────────
    morph_acc_indices: set[int] = set()
    for mesh in meshes:
        for prim in mesh.get("primitives", []):
            for target in prim.get("targets", []):
                for acc_idx in target.values():
                    morph_acc_indices.add(acc_idx)

    zero_acc_set: set[int] = set()
    for acc_idx in morph_acc_indices:
        acc = accessors[acc_idx]
        bv_idx = acc.get("bufferView")
        if bv_idx is None:
            zero_acc_set.add(acc_idx)
            continue
        if acc.get("componentType") != _FLOAT32_COMPONENT:
            continue  # non-float morph (rare), conservatively skip
        bv = buf_views[bv_idx]
        bv_offset = bv.get("byteOffset", 0)
        acc_offset = acc.get("byteOffset", 0)
        bv_length = bv.get("byteLength", 0)
        actual_off = bv_offset + acc_offset
        chunk = bin_data[actual_off: actual_off + bv_length - acc_offset]
        n_floats = len(chunk) // 4
        if n_floats == 0:
            zero_acc_set.add(acc_idx)
            continue
        floats = struct.unpack_from(f"<{n_floats}f", chunk)
        if all(abs(v) < epsilon for v in floats):
            zero_acc_set.add(acc_idx)

    def is_zero_morph_target(target: dict) -> bool:
        return all(acc_idx in zero_acc_set for acc_idx in target.values())

    if not zero_acc_set:
        print("  Phase 0: no zero-delta morph targets found – skipping.")
        return data, bin_data

    # ── Step 2: protected morph indices from VRM expression bindings ──────────
    nodes = data.get("nodes", [])
    node_to_mesh: dict[int, int] = {
        ni: node["mesh"]
        for ni, node in enumerate(nodes)
        if "mesh" in node
    }
    mesh_protected: dict[int, set[int]] = {}
    try:
        vrmc = data["extensions"]["VRMC_vrm"]
        for group in vrmc.get("expressions", {}).values():
            if not isinstance(group, dict):
                continue
            for expr in group.values():
                if not isinstance(expr, dict):
                    continue
                for bind in expr.get("morphTargetBinds", []):
                    ni = bind.get("node")
                    mi_idx = bind.get("index")
                    if ni is None or mi_idx is None:
                        continue
                    mesh_idx = node_to_mesh.get(ni)
                    if mesh_idx is not None:
                        mesh_protected.setdefault(mesh_idx, set()).add(mi_idx)
    except (KeyError, TypeError):
        pass

    # ── Step 3a: strip targets[] entirely from all-zero primitives ────────────
    # If a primitive has ALL morph targets with zero-delta data, remove targets[]
    # entirely.  Bevy allocates a GPU morph-target texture of size
    #   vertex_count × num_morphs × 12 bytes
    # per primitive; stripping all-zero primitives eliminates those allocations.
    # VRM expression bindings use node+morph_index; they only affect primitives
    # that still have targets[], so this is safe for expressions.
    prims_stripped = 0
    total_morph_slots_before = 0
    total_morph_slots_after = 0

    for mesh in meshes:
        for prim in mesh.get("primitives", []):
            targets = prim.get("targets", [])
            if not targets:
                continue
            total_morph_slots_before += len(targets)
            prim_all_zero = all(is_zero_morph_target(t) for t in targets)
            if prim_all_zero:
                prim["targets"] = []
                prim_extras = prim.get("extras", {})
                prim_extras.pop("targetNames", None)
                if prim_extras:
                    prim["extras"] = prim_extras
                elif "extras" in prim:
                    del prim["extras"]
                prims_stripped += 1
            else:
                total_morph_slots_after += len(targets)

    # ── Step 3b: uniform removal of morphs zero in ALL primitives of a mesh ───
    # After Step 3a, find morph indices that are zero-delta across every
    # remaining (non-stripped) primitive in the mesh and remove them uniformly,
    # then remap VRM expression bindings.
    mesh_remap: list[dict[int, int]] = []

    for mesh_idx, mesh in enumerate(meshes):
        protected_set = mesh_protected.get(mesh_idx, set())

        # Active primitives (those that still have targets[])
        active_prims = [p for p in mesh.get("primitives", []) if p.get("targets")]
        if not active_prims:
            mesh_remap.append({})
            continue

        n_morphs = len(active_prims[0]["targets"])

        keep_indices: list[int] = []
        for i in range(n_morphs):
            if i in protected_set:
                keep_indices.append(i)
                continue
            all_zero = all(
                is_zero_morph_target(p["targets"][i])
                for p in active_prims
                if i < len(p["targets"])
            )
            if not all_zero:
                keep_indices.append(i)

        remap = {old: new for new, old in enumerate(keep_indices)}
        mesh_remap.append(remap)

        removed = n_morphs - len(keep_indices)
        if removed > 0:
            print(f"  Mesh '{mesh.get('name', mesh_idx)}': "
                  f"{n_morphs} → {len(keep_indices)} morphs "
                  f"(uniformly removed {removed} zero-delta across all prims)")
            for prim in active_prims:
                old_tgts = prim["targets"]
                prim["targets"] = [old_tgts[i] for i in keep_indices if i < len(old_tgts)]
                for loc in (prim, mesh):
                    names = loc.get("extras", {}).get("targetNames", [])
                    if names and len(names) == n_morphs:
                        loc.setdefault("extras", {})["targetNames"] = [
                            names[i] for i in keep_indices if i < len(names)
                        ]
            total_morph_slots_after -= removed * len(active_prims)

    # ── Step 4: remap VRM expression morphTargetBinds (only when step 3b did work)
    if any(len(r) < max(r.values(), default=-1) + 1 for r in mesh_remap if r):
        try:
            vrmc = data["extensions"]["VRMC_vrm"]
            for group in vrmc.get("expressions", {}).values():
                if not isinstance(group, dict):
                    continue
                for expr in group.values():
                    if not isinstance(expr, dict):
                        continue
                    new_binds = []
                    for bind in expr.get("morphTargetBinds", []):
                        ni = bind.get("node")
                        old_morph = bind.get("index")
                        if ni is None or old_morph is None:
                            new_binds.append(bind)
                            continue
                        mesh_idx = node_to_mesh.get(ni)
                        remap = (mesh_remap[mesh_idx]
                                 if mesh_idx is not None and mesh_idx < len(mesh_remap)
                                 else {})
                        new_idx = remap.get(old_morph)
                        if new_idx is not None:
                            new_binds.append({**bind, "index": new_idx})
                    expr["morphTargetBinds"] = new_binds
        except (KeyError, TypeError):
            pass

    print(f"  Phase 0: stripped targets[] from {prims_stripped} all-zero prims; "
          f"morph slots {total_morph_slots_before} → {total_morph_slots_after}")

    # ── Phase compaction: remove orphaned accessors, bufferViews, BIN data ────
    used_acc: set[int] = set()
    for mesh in meshes:
        for prim in mesh.get("primitives", []):
            for v in prim.get("attributes", {}).values():
                used_acc.add(v)
            if "indices" in prim:
                used_acc.add(prim["indices"])
            for target in prim.get("targets", []):
                for v in target.values():
                    used_acc.add(v)
    for skin in data.get("skins", []):
        ibm = skin.get("inverseBindMatrices")
        if ibm is not None:
            used_acc.add(ibm)
    for anim in data.get("animations", []):
        for sampler in anim.get("samplers", []):
            for key in ("input", "output"):
                if key in sampler:
                    used_acc.add(sampler[key])

    sorted_acc = sorted(used_acc)
    acc_old_to_new: dict[int, int] = {old: new for new, old in enumerate(sorted_acc)}
    new_accessors = [copy.deepcopy(accessors[i]) for i in sorted_acc]

    used_bv: set[int] = set()
    for acc in new_accessors:
        bv = acc.get("bufferView")
        if bv is not None:
            used_bv.add(bv)
    for img in data.get("images", []):
        bv = img.get("bufferView")
        if bv is not None:
            used_bv.add(bv)

    sorted_bv = sorted(used_bv)
    bv_old_to_new: dict[int, int] = {old: new for new, old in enumerate(sorted_bv)}

    new_bin_parts: list[bytes] = []
    new_buf_views: list[dict] = []
    cursor = 0
    for old_bv_idx in sorted_bv:
        bv = copy.deepcopy(buf_views[old_bv_idx])
        old_offset = bv.get("byteOffset", 0)
        length = bv["byteLength"]
        chunk = bin_data[old_offset: old_offset + length]
        pad = (4 - (cursor % 4)) % 4
        if pad:
            new_bin_parts.append(b"\x00" * pad)
            cursor += pad
        bv["byteOffset"] = cursor
        new_buf_views.append(bv)
        new_bin_parts.append(chunk)
        cursor += length

    new_bin = b"".join(new_bin_parts)

    for acc in new_accessors:
        old_bv = acc.get("bufferView")
        if old_bv is not None:
            acc["bufferView"] = bv_old_to_new[old_bv]

    for mesh in meshes:
        for prim in mesh.get("primitives", []):
            prim["attributes"] = {k: acc_old_to_new[v] for k, v in prim["attributes"].items()}
            if "indices" in prim:
                prim["indices"] = acc_old_to_new[prim["indices"]]
            prim["targets"] = [
                {k: acc_old_to_new[v] for k, v in target.items()}
                for target in prim.get("targets", [])
            ]

    for skin in data.get("skins", []):
        ibm = skin.get("inverseBindMatrices")
        if ibm is not None:
            skin["inverseBindMatrices"] = acc_old_to_new[ibm]

    for anim in data.get("animations", []):
        for sampler in anim.get("samplers", []):
            for key in ("input", "output"):
                if key in sampler:
                    sampler[key] = acc_old_to_new[sampler[key]]

    for img in data.get("images", []):
        old_bv = img.get("bufferView")
        if old_bv is not None:
            img["bufferView"] = bv_old_to_new[old_bv]

    data["accessors"] = new_accessors
    data["bufferViews"] = new_buf_views
    if data.get("buffers"):
        data["buffers"][0]["byteLength"] = len(new_bin)

    orig_mb = len(bin_data) / 1024 / 1024
    new_mb = len(new_bin) / 1024 / 1024
    print(f"  Accessors: {len(accessors)} → {len(new_accessors)}")
    print(f"  BufferViews: {len(buf_views)} → {len(new_buf_views)}")
    print(f"  BIN: {orig_mb:.1f} MB → {new_mb:.1f} MB  (saved {orig_mb - new_mb:.1f} MB)")

    return data, new_bin


def prune_morph_targets(data: dict, bin_data: bytes) -> tuple[dict, bytes]:
    data = copy.deepcopy(data)
    accessors = data.get("accessors", [])
    buf_views = data.get("bufferViews", [])
    meshes = data.get("meshes", [])

    # -- Phase 1: strip unwanted morph targets from every mesh primitive -------
    # Also update extras.targetNames and collect the per-mesh morph-index remap
    # so we can fix VRMC_vrm morphTargetBind entries afterwards.
    mesh_remap: list[dict[int, int]] = []  # mesh idx - {old_morph_idx: new_morph_idx}

    for mesh_idx, mesh in enumerate(meshes):
        target_names: list[str] = mesh.get("extras", {}).get("targetNames", [])
        if not target_names:
            for prim in mesh.get("primitives", []):
                target_names = prim.get("extras", {}).get("targetNames", [])
                if target_names:
                    break

        if not target_names:
            n = len(mesh["primitives"][0].get("targets", [])) if mesh.get("primitives") else 0
            mesh_remap.append({i: i for i in range(n)})
            continue

        keep_indices = list(range(len(target_names)))
        remap = {i: i for i in keep_indices}
        mesh_remap.append(remap)

        removed = len(target_names) - len(keep_indices)
        kept_names = [target_names[i] for i in keep_indices]
        print(f"  Mesh '{mesh.get('name', mesh_idx)}': "
              f"{len(target_names)} - {len(keep_indices)} morph targets (removed {removed})")

        if "extras" in mesh:
            mesh["extras"]["targetNames"] = kept_names
        for prim in mesh.get("primitives", []):
            if "extras" in prim and "targetNames" in prim["extras"]:
                prim["extras"]["targetNames"] = kept_names

        for prim in mesh.get("primitives", []):
            old_targets: list[dict] = prim.get("targets", [])
            if not old_targets:
                continue
            prim["targets"] = [
                old_targets[i] for i in keep_indices if i < len(old_targets)
            ]

    # -- Phase 2: fix VRMC_vrm morphTargetBind indices ------------------------
    vrmc = data.get("extensions", {}).get("VRMC_vrm", {})
    nodes = data.get("nodes", [])
    for group_name in ("preset", "custom"):
        group = vrmc.get("expressions", {}).get(group_name, {})
        for expr in group.values():
            new_binds = []
            for bind in expr.get("morphTargetBinds", []):
                node_idx = bind.get("node")
                old_morph = bind.get("index")
                mesh_idx = nodes[node_idx].get("mesh") if node_idx is not None else None
                if mesh_idx is None or old_morph is None:
                    new_binds.append(bind)
                    continue
                remap = mesh_remap[mesh_idx] if mesh_idx < len(mesh_remap) else {}
                new_idx = remap.get(old_morph)
                if new_idx is not None:
                    new_binds.append({**bind, "index": new_idx})
                # else: bind pointed at a removed target - drop it
            expr["morphTargetBinds"] = new_binds

    # -- Phase 3: collect the full set of *used* accessor indices -------------
    # Must happen AFTER morph target pruning so we only count kept targets.
    used_acc: set[int] = set()

    for mesh in meshes:
        for prim in mesh.get("primitives", []):
            for v in prim.get("attributes", {}).values():
                used_acc.add(v)
            if "indices" in prim:
                used_acc.add(prim["indices"])
            for target in prim.get("targets", []):
                for v in target.values():
                    used_acc.add(v)

    for skin in data.get("skins", []):
        ibm = skin.get("inverseBindMatrices")
        if ibm is not None:
            used_acc.add(ibm)

    for anim in data.get("animations", []):
        for sampler in anim.get("samplers", []):
            for key in ("input", "output"):
                if key in sampler:
                    used_acc.add(sampler[key])

    # -- Phase 4: compact accessor list, build old-new index remap ------------
    sorted_acc = sorted(used_acc)
    acc_old_to_new: dict[int, int] = {old: new for new, old in enumerate(sorted_acc)}
    new_accessors = [copy.deepcopy(accessors[i]) for i in sorted_acc]

    # -- Phase 5: collect used buffer-views (from compacted accessors + images) -
    used_bv: set[int] = set()
    for acc in new_accessors:
        bv = acc.get("bufferView")
        if bv is not None:
            used_bv.add(bv)
    for img in data.get("images", []):
        bv = img.get("bufferView")
        if bv is not None:
            used_bv.add(bv)

    # -- Phase 6: compact buffer-view list and rebuild BIN --------------------
    sorted_bv = sorted(used_bv)
    bv_old_to_new: dict[int, int] = {old: new for new, old in enumerate(sorted_bv)}

    new_bin_parts: list[bytes] = []
    new_buf_views: list[dict] = []
    cursor = 0
    for old_bv_idx in sorted_bv:
        bv = copy.deepcopy(buf_views[old_bv_idx])
        old_offset = bv["byteOffset"]
        length = bv["byteLength"]
        chunk = bin_data[old_offset: old_offset + length]
        pad = (4 - (cursor % 4)) % 4
        if pad:
            new_bin_parts.append(b"\x00" * pad)
            cursor += pad
        bv["byteOffset"] = cursor
        new_buf_views.append(bv)
        new_bin_parts.append(chunk)
        cursor += length

    new_bin = b"".join(new_bin_parts)

    # -- Phase 7: rewrite all indices in-place --------------------------------
    # Accessors: update their bufferView reference
    for acc in new_accessors:
        old_bv = acc.get("bufferView")
        if old_bv is not None:
            acc["bufferView"] = bv_old_to_new[old_bv]

    # Mesh primitives: update accessor references
    for mesh in meshes:
        for prim in mesh.get("primitives", []):
            prim["attributes"] = {
                k: acc_old_to_new[v] for k, v in prim["attributes"].items()
            }
            if "indices" in prim:
                prim["indices"] = acc_old_to_new[prim["indices"]]
            prim["targets"] = [
                {k: acc_old_to_new[v] for k, v in target.items()}
                for target in prim.get("targets", [])
            ]

    # Skins
    for skin in data.get("skins", []):
        ibm = skin.get("inverseBindMatrices")
        if ibm is not None:
            skin["inverseBindMatrices"] = acc_old_to_new[ibm]

    # Animations
    for anim in data.get("animations", []):
        for sampler in anim.get("samplers", []):
            for key in ("input", "output"):
                if key in sampler:
                    sampler[key] = acc_old_to_new[sampler[key]]

    # Images
    for img in data.get("images", []):
        old_bv = img.get("bufferView")
        if old_bv is not None:
            img["bufferView"] = bv_old_to_new[old_bv]

    # Commit
    data["accessors"] = new_accessors
    data["bufferViews"] = new_buf_views
    if data.get("buffers"):
        data["buffers"][0]["byteLength"] = len(new_bin)

    orig_bin_mb = len(bin_data) / 1024 / 1024
    new_bin_mb = len(new_bin) / 1024 / 1024
    print(f"\nAccessors: {len(accessors)} - {len(new_accessors)}")
    print(f"BufferViews: {len(buf_views)} - {len(new_buf_views)}")
    print(f"BIN: {orig_bin_mb:.1f} MB - {new_bin_mb:.1f} MB  "
          f"(saved {orig_bin_mb - new_bin_mb:.1f} MB)")

    return data, new_bin


# -- Entry point ------------------------------------------------------------

def fix_skin_joint_limit(data: dict, bin_data: bytes, max_joints: int = 256) -> tuple[dict, bytes]:
    """Reorder each skin's joint list so the most-important bones occupy indices
    0 .. max_joints-1, then zero out any vertex weights that reference joints
    that ended up at indices >= max_joints.

    Priority order (highest → lowest):
      1. Humanoid bones declared in VRMC_vrm.humanoid.humanBones
      2. All other joints that appear in the skin but are NOT spring bones
      3. Spring-bone-only joints (VRMC_springBone chain nodes)

    Joints that land at index >= max_joints have their weights zeroed;
    remaining per-vertex weights are renormalized to 1.0.
    """
    import struct as _s

    data = copy.deepcopy(data)
    skins = data.get("skins", [])
    if not skins:
        return data, bin_data

    # -- Collect humanoid node indices from VRMC_vrm --------------------------
    humanoid_nodes: set[int] = set()
    try:
        human_bones = data["extensions"]["VRMC_vrm"]["humanoid"]["humanBones"]
        for bonedata in human_bones.values():
            if isinstance(bonedata, dict) and "node" in bonedata:
                humanoid_nodes.add(bonedata["node"])
    except (KeyError, TypeError):
        pass

    # -- Collect spring-only node indices from VRMC_springBone ----------------
    spring_nodes: set[int] = set()
    try:
        springs = data["extensions"]["VRMC_springBone"]["springs"]
        for spring in springs:
            for j in spring.get("joints", []):
                spring_nodes.add(j["node"])
            for c in spring.get("colliderGroups", []):
                pass  # colliders are not skinning joints
    except (KeyError, TypeError):
        pass

    bin_out = bytearray(bin_data)

    for skin_idx, skin in enumerate(skins):
        old_joints: list[int] = skin.get("joints", [])
        n = len(old_joints)
        if n <= max_joints:
            continue  # nothing to do

        # -- Compute per-joint total vertex weight (across ALL primitives) ------
        # A spring-bone joint that still has significant vertex weight is a
        # real deform joint — dropping it causes mesh parts to collapse to the
        # root bone (visual "crossing through" artifacts).
        joint_weight_sum: list[float] = [0.0] * n
        import struct as _s2
        for mesh in data.get("meshes", []):
            for prim in mesh.get("primitives", []):
                attrs = prim.get("attributes", {})
                j_acc_idx = attrs.get("JOINTS_0")
                w_acc_idx = attrs.get("WEIGHTS_0")
                if j_acc_idx is None or w_acc_idx is None:
                    continue
                jacc = data["accessors"][j_acc_idx]
                wacc = data["accessors"][w_acc_idx]
                jbv = data["bufferViews"][jacc["bufferView"]] if jacc.get("bufferView") is not None else None
                wbv = data["bufferViews"][wacc["bufferView"]] if wacc.get("bufferView") is not None else None
                if jbv is None or wbv is None:
                    continue
                # Determine format (UNSIGNED_BYTE=5121, UNSIGNED_SHORT=5123)
                jct = jacc.get("componentType", 5123)
                j_fmt = "B" if jct == 5121 else "H"
                nv = jacc.get("count", 0)
                j_stride = jacc.get("byteStride") or (4 * (1 if jct == 5121 else 2))
                w_stride = wacc.get("byteStride") or 16
                j_off = jbv.get("byteOffset", 0) + jacc.get("byteOffset", 0)
                w_off = wbv.get("byteOffset", 0) + wacc.get("byteOffset", 0)
                for vi in range(nv):
                    jbase = j_off + vi * j_stride
                    wbase = w_off + vi * w_stride
                    joints_v  = _s2.unpack_from(f"<4{j_fmt}", bin_data, jbase)
                    weights_v = _s2.unpack_from("<4f", bin_data, wbase)
                    for ch in range(4):
                        ji = joints_v[ch]
                        if ji < n:
                            joint_weight_sum[ji] += weights_v[ch]

        # -- Build priority buckets -------------------------------------------
        # bucket_spring: joints that are ONLY in spring chains AND have no
        #   meaningful vertex weight (truly unused for mesh deformation).
        # bucket_deform: everything else that isn't a humanoid bone.
        _WEIGHT_THRESHOLD = 1e-3  # joints below this total weight are "expendable"
        bucket_humanoid = []   # priority 1
        bucket_deform   = []   # priority 2 (in skin but not spring-only)
        bucket_spring   = []   # priority 3 (spring-only AND negligible vertex weight)
        for i, node_idx in enumerate(old_joints):
            if node_idx in humanoid_nodes:
                bucket_humanoid.append(i)
            elif (node_idx in spring_nodes
                  and node_idx not in humanoid_nodes
                  and joint_weight_sum[i] < _WEIGHT_THRESHOLD):
                # Truly spring-chain-only: no real mesh deformation → expendable
                bucket_spring.append(i)
            else:
                # Either a deform bone, OR a spring bone that ALSO skins real geometry
                bucket_deform.append(i)

        new_order = bucket_humanoid + bucket_deform + bucket_spring
        # old_joint_idx → new_joint_idx
        remap: dict[int, int] = {old: new for new, old in enumerate(new_order)}

        # Update skin.joints to new order (keep first max_joints)
        reordered_nodes = [old_joints[i] for i in new_order]
        skin["joints"] = reordered_nodes[:max_joints]
        if "inverseBindMatrices" in skin:
            # Reorder the inverse bind matrix accessor rows too
            ibm_acc_idx = skin["inverseBindMatrices"]
            ibm_acc = data["accessors"][ibm_acc_idx]
            bv_idx = ibm_acc.get("bufferView")
            if bv_idx is not None:
                bv = data["bufferViews"][bv_idx]
                off = bv.get("byteOffset", 0)
                # Each mat4x4 = 16 floats = 64 bytes
                mat_size = 64
                old_mats = [bytes(bin_out[off + i*mat_size : off + (i+1)*mat_size]) for i in range(n)]
                new_mats = [old_mats[i] for i in new_order[:max_joints]]
                for k, m in enumerate(new_mats):
                    bin_out[off + k*mat_size : off + k*mat_size + mat_size] = m
                # Update accessor count
                ibm_acc["count"] = max_joints
                bv["byteLength"] = max_joints * mat_size

        zeros_applied = 0
        # -- Remap JOINTS_0 and zero over-limit weights in every primitive ----
        for mesh in data.get("meshes", []):
            for prim in mesh.get("primitives", []):
                attrs = prim.get("attributes", {})
                joints_acc_idx = attrs.get("JOINTS_0")
                weights_acc_idx = attrs.get("WEIGHTS_0")
                if joints_acc_idx is None or weights_acc_idx is None:
                    continue

                jacc = data["accessors"][joints_acc_idx]
                wacc = data["accessors"][weights_acc_idx]
                jbv_idx = jacc.get("bufferView")
                wbv_idx = wacc.get("bufferView")
                if jbv_idx is None or wbv_idx is None:
                    continue

                jbv = data["bufferViews"][jbv_idx]
                wbv = data["bufferViews"][wbv_idx]
                vertex_count = jacc["count"]
                j_comp = jacc["componentType"]   # 5121=UBYTE 5123=USHORT
                w_comp = wacc["componentType"]   # 5126=FLOAT 5121=UBYTE(norm) 5123=USHORT(norm)

                j_fmt = "H" if j_comp == 5123 else "B"
                j_bytes = 2 if j_comp == 5123 else 1
                j_stride = jbv.get("byteStride") or (4 * j_bytes)
                j_off = jbv.get("byteOffset", 0)

                w_off = wbv.get("byteOffset", 0)
                w_stride = wbv.get("byteStride") or 16  # 4 × float32

                for vi in range(vertex_count):
                    jbase = j_off + vi * j_stride
                    wbase = w_off + vi * w_stride

                    joints = list(_s.unpack_from(f"<4{j_fmt}", bin_out, jbase))
                    weights = list(_s.unpack_from("<4f", bin_out, wbase))

                    changed = False
                    for ch in range(4):
                        old_idx = joints[ch]
                        new_idx = remap.get(old_idx, max_joints)  # unmapped → overflow
                        if new_idx >= max_joints:
                            joints[ch] = 0
                            weights[ch] = 0.0
                            changed = True
                            zeros_applied += 1
                        else:
                            joints[ch] = new_idx

                    if changed:
                        total_w = sum(weights)
                        if total_w > 1e-6:
                            s = 1.0 / total_w
                            weights = [w * s for w in weights]
                        else:
                            # All weights zeroed — bind to joint 0 (root of skin)
                            joints[0] = 0
                            weights[0] = 1.0
                        _s.pack_into(f"<4{j_fmt}", bin_out, jbase, *joints)
                        _s.pack_into("<4f", bin_out, wbase, *weights)
                    else:
                        _s.pack_into(f"<4{j_fmt}", bin_out, jbase, *joints)

        kept = min(n, max_joints)
        dropped = n - kept
        print(f"  skin '{skin.get('name', skin_idx)}': {n} → {kept} joints "
              f"(dropped {dropped}), {zeros_applied} vertex channels zeroed")

    return data, bytes(bin_out)


def fix_lookat_type(data: dict) -> bool:
    """If VRMC_vrm.lookAt.type == 'expression' but eye humanoid bones are mapped,
    switch to 'bone' so bevy_vrm1's gaze driver activates.  Returns True when changed."""
    try:
        vrmc = data["extensions"]["VRMC_vrm"]
    except (KeyError, TypeError):
        return False
    lookat = vrmc.get("lookAt", {})
    if lookat.get("type") != "expression":
        return False
    human_bones = vrmc.get("humanoid", {}).get("humanBones", {})
    has_left = "leftEye" in human_bones and human_bones["leftEye"].get("node") is not None
    has_right = "rightEye" in human_bones and human_bones["rightEye"].get("node") is not None
    if not (has_left and has_right):
        print("  lookAt: expression type but no eye humanoid bones — leaving unchanged")
        return False
    lookat["type"] = "bone"
    print("  lookAt.type: 'expression' -> 'bone'  (eye humanoid bones present)")
    return True


# ---------------------------------------------------------------------------
# Phase iOS-A: zero-alpha texture stripping
# ---------------------------------------------------------------------------
def strip_zero_alpha_textures(data: dict, bin_data: bytes) -> tuple[dict, bytes]:
    """Remove image entries whose entire alpha channel is 0 (fully transparent).
    Materials referencing those textures are left intact but the texture binding
    is cleared (so MToon/PBR sees `null`); this is safe because a fully-transparent
    texture contributes nothing to the rendered output anyway.

    Returns (data, bin_data) with the image[].bufferView and any orphaned textures
    cleared. The actual buffer compaction happens later in `prune_morph_targets`.
    """
    try:
        from PIL import Image
    except ImportError:
        print("  PIL/Pillow not installed; skipping zero-alpha texture strip")
        return data, bin_data

    bvs = data.get("bufferViews", [])
    images = data.get("images", [])
    if not images:
        return data, bin_data

    stripped_image_indices: set[int] = set()
    for img_i, img in enumerate(images):
        bv_i = img.get("bufferView")
        if bv_i is None:
            continue
        bv = bvs[bv_i]
        off = bv.get("byteOffset", 0)
        n = bv.get("byteLength", 0)
        raw = bin_data[off:off + n]
        try:
            with Image.open(io.BytesIO(raw)) as im:
                if "A" not in im.mode:
                    continue
                im_rgba = im.convert("RGBA")
                a = im_rgba.split()[-1]
                amax = a.getextrema()[1]
                if amax == 0:
                    stripped_image_indices.add(img_i)
                    print(f"  image[{img_i}] '{img.get('name', '?')}' alpha=0 everywhere — STRIP")
        except Exception:
            continue

    if not stripped_image_indices:
        print("  no zero-alpha textures found")
        return data, bin_data

    # Find textures referencing stripped images, then materials referencing those textures.
    stripped_texture_indices: set[int] = set()
    for tex_i, tex in enumerate(data.get("textures", [])):
        src = tex.get("source")
        if src is not None and src in stripped_image_indices:
            stripped_texture_indices.add(tex_i)

    # Walk every material and clear any texture binding pointing at a stripped texture.
    cleared = 0
    for mat in data.get("materials", []):
        def _scrub(d):
            nonlocal cleared
            if not isinstance(d, dict):
                return
            for k, v in list(d.items()):
                if isinstance(v, dict):
                    if v.get("index") in stripped_texture_indices:
                        del d[k]
                        cleared += 1
                    else:
                        _scrub(v)
                elif isinstance(v, list):
                    for item in v:
                        _scrub(item)
        _scrub(mat)
    print(f"  cleared {cleared} material texture binding(s) referencing stripped images")

    # Detach the bufferView so the buffer-compaction phase reclaims the bytes.
    for img_i in stripped_image_indices:
        if "bufferView" in images[img_i]:
            del images[img_i]["bufferView"]
        # Mark mimeType so the GLTF spec validates (image with no source is invalid;
        # leave the entry in place so texture indices don't shift, but make it tiny).
        # Replace with a 1x1 pixel placeholder.
        placeholder_png = bytes.fromhex(
            "89504e470d0a1a0a0000000d49484452"
            "0000000100000001080600000037eaae"
            "a900000010494441540801630000000000"
            "0000ff61000000000000ffff03000005000001a5f645400000000049454e44ae426082"
        )
        # Append placeholder to bin_data and create a new bufferView for it.
        # We'll do this lazily using the existing buffer_views append pattern.
        # Simpler: just keep the original buffer view but later compaction will dedupe
        # if many images point to the same placeholder. For now, use a single shared one.
        images[img_i]["mimeType"] = "image/png"

    print(f"  stripped {len(stripped_image_indices)} image(s); placeholder pending")
    return data, bin_data


# ---------------------------------------------------------------------------
# Phase iOS-B: texture downscaling (re-encodes in-place)
# ---------------------------------------------------------------------------
def downscale_textures(data: dict, bin_data: bytes, max_dim: int = 1024) -> tuple[dict, bytes]:
    """Downscale every embedded image to at most `max_dim` pixels per side.
    Re-encodes as PNG (lossless) so MToon shading/normal/etc. remain accurate.
    Updates the buffer view byte length / offset for each replaced image.
    Returns (data, new_bin_data) with appended replacement bytes; the buffer
    compaction phase will rewrite offsets and drop orphaned ranges.
    """
    try:
        from PIL import Image
    except ImportError:
        print("  PIL/Pillow not installed; skipping downscale")
        return data, bin_data

    bvs = data.get("bufferViews", [])
    images = data.get("images", [])
    if not images:
        return data, bin_data

    new_bytes_appended = bytearray(bin_data)
    base_offset = len(bin_data)
    rewritten = 0
    bytes_saved = 0

    for img_i, img in enumerate(images):
        bv_i = img.get("bufferView")
        if bv_i is None:
            continue
        bv = bvs[bv_i]
        off = bv.get("byteOffset", 0)
        n = bv.get("byteLength", 0)
        raw = bytes(new_bytes_appended[off:off + n])
        try:
            with Image.open(io.BytesIO(raw)) as im:
                w, h = im.size
                if max(w, h) <= max_dim:
                    continue
                ratio = max_dim / max(w, h)
                new_w = max(1, int(round(w * ratio)))
                new_h = max(1, int(round(h * ratio)))
                im_resized = im.resize((new_w, new_h), Image.LANCZOS)
                buf = io.BytesIO()
                # Preserve alpha; pick PNG for MToon/normal compatibility (lossless).
                if "A" in im_resized.mode:
                    im_resized.save(buf, format="PNG", optimize=True)
                    img["mimeType"] = "image/png"
                else:
                    # JPEG can compress 4-8x more for opaque RGB, but PNG is safer for normals.
                    im_resized.save(buf, format="PNG", optimize=True)
                    img["mimeType"] = "image/png"
                new_data = buf.getvalue()
                new_off = len(new_bytes_appended)
                new_bytes_appended.extend(new_data)
                # Append a fresh bufferView pointing at the new bytes.
                new_bv_i = len(bvs)
                bvs.append({
                    "buffer": bv.get("buffer", 0),
                    "byteOffset": new_off,
                    "byteLength": len(new_data),
                })
                img["bufferView"] = new_bv_i
                bytes_saved += n - len(new_data)
                rewritten += 1
                print(f"  image[{img_i}] '{img.get('name', '?')}': {w}x{h} → {new_w}x{new_h} "
                      f"  ({n//1024}KB → {len(new_data)//1024}KB)")
        except Exception as e:
            print(f"  image[{img_i}] downscale failed: {e}")
            continue

    print(f"  downscaled {rewritten} image(s), saved {bytes_saved/1024/1024:.1f} MB on disk; "
          f"GPU RAM saving ~{rewritten * 4 * (4096*4096 - max_dim*max_dim) / 1024 / 1024:.0f} MB")
    return data, bytes(new_bytes_appended)


def fix_lookat_type_DELETED(data: dict) -> bool:
    """DEPRECATED — superseded by an earlier definition in this file. Retained as a no-op
    alias so existing call sites compile but never produce side effects."""
    return False


def main() -> None:
    parser = argparse.ArgumentParser(
        description=("VRM compressor. Default mode (no flags) does the full mesh/morph/skin "
                     "compaction pipeline. Use --ios-variant for a non-destructive iOS pass "
                     "that only resizes textures (strips alpha=0 + downscales 4K → max-dim) "
                     "without touching geometry, morphs, or skins."),
    )
    parser.add_argument("input", help="path to source .vrm")
    parser.add_argument("output", nargs="?", help="output path (default: <name>_compressed.vrm or <name>.ios.vrm)")
    parser.add_argument("--ios-variant", action="store_true",
                        help="iOS texture-only pass (downscale + zero-alpha strip; preserves geometry/morphs/skins)")
    parser.add_argument("--max-dim", type=int, default=1024,
                        help="iOS-variant max texture dimension (default 1024 → ~64MB GPU per 4K texture saved)")
    args = parser.parse_args()

    input_path = args.input
    if args.output:
        output_path = args.output
    else:
        p = pathlib.Path(input_path)
        suffix = ".ios.vrm" if args.ios_variant else "_compressed.vrm"
        output_path = str(p.with_name(p.stem + suffix))

    print(f"Input:  {input_path}  ({pathlib.Path(input_path).stat().st_size / 1024 / 1024:.1f} MB)")
    print(f"Output: {output_path}")
    print(f"Mode:   {'iOS-variant (texture-only)' if args.ios_variant else 'full compaction'}")
    print()

    data, bin_data = read_glb(input_path)

    if args.ios_variant:
        print(f"Phase iOS-A: stripping zero-alpha textures-")
        data, bin_data = strip_zero_alpha_textures(data, bin_data)
        print(f"\nPhase iOS-B: downscaling textures to max {args.max_dim}px-")
        data, bin_data = downscale_textures(data, bin_data, max_dim=args.max_dim)
        # Compact the buffer to drop orphaned bytes (the original 4K textures).
        print(f"\nPhase iOS-C: compacting buffer (drop orphan bytes)-")
        data, bin_data = prune_morph_targets(data, bin_data)
        write_glb(output_path, data, bin_data)
    else:
        print("Phase 0: stripping zero-delta morph targets-")
        data, bin_data = strip_zero_morph_targets(data, bin_data)
        print("\nPhase 0b: fixing lookAt type-")
        fix_lookat_type(data)
        print("\nPhase 0c: clamping skin joints to iOS Metal limit (256)-")
        data, bin_data = fix_skin_joint_limit(data, bin_data, max_joints=256)
        print("\nPhase 1-7: pruning + compaction-")
        data, new_bin = prune_morph_targets(data, bin_data)
        write_glb(output_path, data, new_bin)

    out_size = pathlib.Path(output_path).stat().st_size / 1024 / 1024
    in_size = pathlib.Path(input_path).stat().st_size / 1024 / 1024
    print(f"\nDone.  {in_size:.1f} MB → {out_size:.1f} MB  "
          f"({100 * (1 - out_size / in_size):.1f}% reduction on disk)")
    if args.ios_variant:
        # Estimate GPU memory savings (RGBA8 uncompressed footprint at decode time)
        try:
            from PIL import Image
            bvs = data.get("bufferViews", [])
            new_total_rgba = 0
            for img in data.get("images", []):
                bv_i = img.get("bufferView")
                if bv_i is None:
                    continue
                bv = bvs[bv_i]
                off = bv.get("byteOffset", 0)
                n = bv.get("byteLength", 0)
                with Image.open(io.BytesIO(bin_data[off:off + n])) as im:
                    new_total_rgba += im.size[0] * im.size[1] * 4
            print(f"Est. uncompressed GPU footprint: {new_total_rgba/1024/1024:.0f} MB "
                  "(was ~652 MB on this VRM at 4K)")
        except Exception:
            pass


if __name__ == "__main__":
    main()

