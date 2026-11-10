//! The cosmetics list (`data/cosmetics.json` layout) built from the installation: the MainMenu scene's
//! CosmeticsManager (suits / hats / cigarettes parents, `mats` / `fparmsMats` / `cigs` arrays) and the
//! CosmeticInstance components under each parent, in `GetComponentsInChildren` order.

use crate::owned::Owned;
use serde_json::{Value, json};

/// "asset:<Dir>/<file>" -> "<Dir>/<file>".
fn asset(v: &Value) -> Option<String> {
    v.as_str()?.strip_prefix("asset:").map(str::to_string)
}

fn stem(p: &str) -> String {
    let f = p.rsplit('/').next().unwrap_or(p);
    f.rsplit_once('.').map(|(a, _)| a).unwrap_or(f).to_string()
}

/// File names as the old copies were named (spaces, brackets and accents replaced, repeated '_' collapsed).
fn clean(s: &str) -> String {
    let s: String = s
        .chars()
        .map(|c| match c {
            ' ' | '(' | ')' => '_',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'É' | 'È' | 'Ê' => 'E',
            'à' | 'â' | 'ä' => 'a',
            'ç' => 'c',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            c => c,
        })
        .collect();
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if !(c == '_' && out.ends_with('_')) {
            out.push(c);
        }
    }
    out
}

fn flag(ci: &Value, k: &str) -> bool {
    match &ci[k] {
        Value::Bool(b) => *b,
        v => v.as_i64().unwrap_or(0) != 0,
    }
}

fn unlock(ci: &Value) -> &'static str {
    if flag(ci, "unlockWithSupporterDlc") {
        "supporter"
    } else if flag(ci, "unlockWithDlc") {
        "dlc"
    } else if flag(ci, "unlockWithChallenge") {
        "challenge"
    } else if flag(ci, "unlockWithXp") {
        "xp"
    } else {
        "free"
    }
}

/// Material -> { material, texture, color, normal }.
fn mat_info(owned: &Owned, m: &Value) -> Value {
    let Some(mat) = asset(m) else { return Value::Null };
    let Some(j) = owned.material_json(&stem(&mat)) else { return Value::Null };
    let tex = |k: &str| -> Option<String> {
        let p = j["tex"].get(k)?["path"].as_str()?;
        Some(format!("cosmetics/textures/{}", clean(p.rsplit('/').next()?)))
    };
    let c = &j["colors"]["_Color"];
    let col = |i: usize| c.get(i).and_then(|x| x.as_f64()).unwrap_or(1.0);
    json!({
        "material": mat,
        "texture": tex("_BC").or_else(|| tex("_MainTex")).unwrap_or_default(),
        "color": [col(0), col(1), col(2), col(3)],
        "normal": tex("_normal"),
    })
}

/// Root MeshCollider mesh of a hat prefab -> "cosmetics/hat_colliders/<mesh>.asset".
fn hat_collider(owned: &Owned, prefab: &str) -> Option<String> {
    let p = owned.prefab_json(&stem(prefab), "sharedassets").ok()?;
    let root = p["objects"].as_array()?.first()?;
    let mc = root["components"].as_array()?.iter().find(|c| c["type"] == "MeshCollider")?;
    let mesh = asset(&mc["fields"]["m_Mesh"])?;
    Some(format!("cosmetics/hat_colliders/{}", clean(mesh.rsplit('/').next()?)))
}

pub fn cosmetics_json(owned: &Owned) -> Result<Value, String> {
    let level = owned.scene_level("MainMenu").ok_or("MainMenu not in BuildSettings")?;
    let scene = crate::prefab_json::scene_json(owned, owned.assemblies().ok_or("no managed assemblies")?, level)?;
    let objects = scene["objects"].as_array().ok_or("no objects")?;
    let comp = |o: &Value, ty: &str| -> Option<Value> { o["components"].as_array()?.iter().find(|c| c["type"] == ty).map(|c| c["fields"].clone()) };
    let cm = objects.iter().find_map(|o| comp(o, "CosmeticsManager")).ok_or("CosmeticsManager not found in MainMenu")?;
    // GetComponentsInChildren<CosmeticInstance>(): depth first in sibling order, inactive objects (and below) skipped
    let children = |k: &str| -> Vec<Value> {
        let Some(root) = cm[k].as_str().and_then(|r| r.strip_prefix("ref:")).map(|r| r.split("::").next().unwrap_or(r).to_string()) else { return vec![] };
        let mut out = Vec::new();
        let mut inactive: Vec<String> = Vec::new();
        for o in objects {
            let path = o["path"].as_str().unwrap_or("");
            if path != root && !path.starts_with(&format!("{root}/")) {
                continue;
            }
            if inactive.iter().any(|p| path.starts_with(&format!("{p}/"))) {
                continue;
            }
            if o["active"].as_bool() == Some(false) {
                inactive.push(path.to_string());
                continue;
            }
            out.extend(comp(o, "CosmeticInstance"));
        }
        out
    };
    let name = |ci: &Value| ci["cosmeticName"].as_str().unwrap_or("").to_string();
    let sprite = |ci: &Value| asset(&ci["sprite"]);
    let seq = |k: &str| cm[k].as_array().cloned().unwrap_or_default();

    let (mats, arms) = (seq("mats"), seq("fparmsMats"));
    let suits: Vec<Value> = children("suitsParent")
        .iter()
        .enumerate()
        .map(|(i, ci)| {
            json!({
                "index": i,
                "name": name(ci),
                "sprite": sprite(ci),
                "unlock": unlock(ci),
                "body": mats.get(i).map(|m| mat_info(owned, m)).unwrap_or(Value::Null),
                "arms": arms.get(i).map(|m| mat_info(owned, m)).unwrap_or(Value::Null),
            })
        })
        .collect();
    let item = |i: usize, ci: &Value, prefab: Option<String>, dir: &str, collider: bool| -> Value {
        let prefab = prefab.unwrap_or_default();
        let mut v = json!({ "index": i, "name": name(ci), "prefab": prefab, "model": format!("{dir}{}.glb", clean(&stem(&prefab))), "sprite": sprite(ci), "unlock": unlock(ci) });
        if collider {
            if let Some(c) = hat_collider(owned, &prefab) {
                v["collider"] = json!(c);
            }
        }
        v
    };
    let hats: Vec<Value> = children("hatsParent").iter().enumerate().map(|(i, ci)| item(i, ci, asset(&ci["hat"]), "cosmetics/hats/", true)).collect();
    let cig_prefabs = seq("cigs");
    let cigs: Vec<Value> = children("cigsParent").iter().enumerate().map(|(i, ci)| item(i, ci, cig_prefabs.get(i).and_then(asset), "cosmetics/cigs/", false)).collect();
    if suits.is_empty() || hats.is_empty() || cigs.is_empty() {
        return Err(format!("cosmetics incomplete: {} suits, {} hats, {} cigs", suits.len(), hats.len(), cigs.len()));
    }
    let body_parts = ["SK_Aboubi00_Arm_Left00", "SK_Aboubi00_Arm_Right00", "SK_Aboubi00_Leg_Left00", "SK_Aboubi00_Leg_Right00", "SM_Aboubi00_Torso"];
    let mut body: Vec<Value> = body_parts.iter().map(|n| json!({ "node": n, "model": format!("cosmetics/body/{n}_0.glb"), "material": "suit" })).collect();
    for n in ["SK_Aboubi00_Hand_Left00", "SK_Aboubi00_Hand_Right00", "SM_Aboubi_Head00"] {
        body.push(json!({ "node": n, "model": format!("cosmetics/body/{n}.glb"), "material": "face" }));
    }
    Ok(json!({
        "hat_pivot": "HatPivot",
        "body_nodes_suit": body_parts,
        "face_texture": "cosmetics/textures/T_Boubi_Face00.png",
        "body": body,
        "suits": suits,
        "hats": hats,
        "cigs": cigs,
    }))
}

#[cfg(test)]
mod tests {
    /// The list built in place equals the old generated file (RESTRAFE_COSMETICS_REF, default assets/data/cosmetics.json).
    #[test]
    fn cosmetics_match_export() {
        let Ok(owned) = crate::owned::Owned::open() else { return };
        let path = std::env::var("RESTRAFE_COSMETICS_REF").unwrap_or(crate::owned::reference_file("assets/data/cosmetics.json"));
        let Ok(t) = std::fs::read_to_string(&path) else { return };
        let r: serde_json::Value = serde_json::from_str(&t).unwrap();
        let t0 = std::time::Instant::now();
        let g = super::cosmetics_json(&owned).unwrap();
        println!("built in {:?}", t0.elapsed());
        let mut bad = 0;
        for k in ["hat_pivot", "body_nodes_suit", "face_texture", "body", "suits", "hats", "cigs"] {
            match (r[k].as_array(), g[k].as_array()) {
                (Some(a), Some(b)) => {
                    if a.len() != b.len() {
                        println!("{k}: {} vs {} entries", a.len(), b.len());
                        bad += 1;
                    }
                    for (x, y) in a.iter().zip(b) {
                        if x != y {
                            bad += 1;
                            println!("{k}: {x}\n   | {y}");
                        }
                    }
                }
                _ => {
                    if r[k] != g[k] {
                        bad += 1;
                        println!("{k}: {} | {}", r[k], g[k]);
                    }
                }
            }
        }
        println!("cosmetics differences: {bad}");
    }
}
