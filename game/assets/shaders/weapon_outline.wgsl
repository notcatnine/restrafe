// S_WeaponOutline_00 (Amplify outline pass): inverted hull. Vertices pushed along their normal by
// _OutlineWidth (object space), front faces culled, flat _Color_Outline.
#import bevy_pbr::mesh_functions::{get_world_from_local, mesh_position_local_to_clip}

struct OutlineMat {
    color: vec4<f32>,
    width: f32,
};

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> material: OutlineMat;

struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
};

@vertex
fn vertex(v: Vertex) -> VertexOutput {
    var out: VertexOutput;
    let p = v.position + normalize(v.normal) * material.width;
    out.clip_position = mesh_position_local_to_clip(get_world_from_local(v.instance_index), vec4<f32>(p, 1.0));
    return out;
}

@fragment
fn fragment() -> @location(0) vec4<f32> {
    return material.color;
}
