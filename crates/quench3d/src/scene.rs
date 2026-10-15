//! 场景：把"网格 + 材质 + 变换 + 可选物理刚体"组织成一个可提交给渲染器的列表。
//!
//! 这是引擎里最贴近"搭场景"的抽象：加一个物体、挂一个刚体、
//! 每帧自动从物理世界同步位姿，然后一次性提交绘制。

use std::collections::HashMap;

use quench3d_math::{Transform, Vec3, Vec4};
use quench3d_physics::{BodyHandle, PhysicsWorld};
use quench3d_render::{InstanceDesc, Lighting, MaterialDesc, MaterialId, MeshId, Renderer};

/// 场景中的一个实例。
#[derive(Clone, Debug)]
pub struct SceneObject {
    /// 名称（调试与查找用）。
    pub name: String,
    /// 网格句柄。
    pub mesh: MeshId,
    /// 材质句柄。
    pub material: MaterialId,
    /// 世界变换。
    pub transform: Transform,
    /// 关联的物理刚体（`None` 表示纯静态显示）。
    pub body: Option<BodyHandle>,
    /// 是否可见。
    pub visible: bool,
    /// 颜色倍乘（线性 RGBA）。
    pub color: Vec4,
    /// 金属度覆盖。
    pub metallic: Option<f32>,
    /// 粗糙度覆盖。
    pub roughness: Option<f32>,
    /// 自发光强度。
    pub emissive_strength: f32,
}

impl SceneObject {
    /// 生成绘制描述。
    pub fn instance(&self) -> InstanceDesc {
        let mut d = InstanceDesc::new(self.transform).with_color(self.color);
        d.emissive_strength = self.emissive_strength;
        if self.metallic.is_some() || self.roughness.is_some() {
            d = d.with_pbr(self.metallic.unwrap_or(0.0), self.roughness.unwrap_or(0.5));
        }
        d
    }
}

/// 场景。
pub struct Scene {
    /// 全部物体。
    pub objects: Vec<SceneObject>,
    /// 光照与天空环境。
    pub lighting: Lighting,
    /// 物理步进后是否自动把刚体位姿同步到物体。
    pub auto_sync_physics: bool,
}

impl Default for Scene {
    fn default() -> Self {
        Self::new()
    }
}

impl Scene {
    /// 空场景。
    pub fn new() -> Self {
        Self {
            objects: Vec::new(),
            lighting: Lighting::default(),
            auto_sync_physics: true,
        }
    }

    /// 添加一个物体。
    pub fn add(&mut self, mesh: MeshId, material: MaterialId, transform: Transform) -> usize {
        self.add_named(mesh, material, transform, "object")
    }

    /// 添加并命名。
    pub fn add_named(
        &mut self,
        mesh: MeshId,
        material: MaterialId,
        transform: Transform,
        name: &str,
    ) -> usize {
        let index = self.objects.len();
        self.objects.push(SceneObject {
            name: name.to_string(),
            mesh,
            material,
            transform,
            body: None,
            visible: true,
            color: Vec4::new(1.0, 1.0, 1.0, 1.0),
            metallic: None,
            roughness: None,
            emissive_strength: 1.0,
        });
        index
    }

    /// 添加并绑定物理刚体。
    pub fn add_with_body(
        &mut self,
        mesh: MeshId,
        material: MaterialId,
        body: BodyHandle,
        world: &PhysicsWorld,
        name: &str,
    ) -> usize {
        let transform = world.body(body).map(|b| b.transform()).unwrap_or_default();
        let index = self.add_named(mesh, material, transform, name);
        self.objects[index].body = Some(body);
        index
    }

    /// 按名称查找。
    pub fn find(&self, name: &str) -> Option<usize> {
        self.objects.iter().position(|o| o.name == name)
    }

    /// 按名称获取可变引用。
    pub fn get_mut(&mut self, name: &str) -> Option<&mut SceneObject> {
        self.objects.iter_mut().find(|o| o.name == name)
    }

    /// 按刚体句柄查找物体下标。
    pub fn find_by_body(&self, body: BodyHandle) -> Option<usize> {
        self.objects.iter().position(|o| o.body == Some(body))
    }

    /// 清空。
    pub fn clear(&mut self) {
        self.objects.clear();
    }

    /// 物体数量。
    pub fn len(&self) -> usize {
        self.objects.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    /// 从物理世界同步所有已绑定物体的位姿（保留缩放）。
    pub fn sync_from_physics(&mut self, world: &PhysicsWorld) {
        for obj in &mut self.objects {
            if let Some(handle) = obj.body {
                if let Some(body) = world.body(handle) {
                    let scale = obj.transform.scale;
                    obj.transform = body.transform();
                    obj.transform.scale = scale;
                }
            }
        }
    }

    /// 把物体位姿写回物理世界（移动静态物体或做"抓取"时有用）。
    pub fn sync_to_physics(&self, world: &mut PhysicsWorld) {
        for obj in &self.objects {
            if let Some(handle) = obj.body {
                if let Some(body) = world.body_mut(handle) {
                    body.set_transform(obj.transform);
                }
            }
        }
    }

    /// 提交全部可见物体到渲染器。
    pub fn submit(&self, renderer: &mut Renderer) {
        for obj in &self.objects {
            if obj.visible {
                renderer.draw(obj.mesh, obj.material, obj.instance());
            }
        }
    }

    /// 场景包围盒（用于自动对焦相机与自适应阴影范围）。
    /// 一组物体的合并包围盒（用于对准相机或做范围查询）。
    pub fn bounds_of(&self, renderer: &Renderer, indices: &[usize]) -> Option<quench3d_math::Aabb> {
        let mut bounds = quench3d_math::Aabb::EMPTY;
        for &i in indices {
            let Some(obj) = self.objects.get(i) else {
                continue;
            };
            let (_, _, local) = renderer.mesh_info(obj.mesh)?;
            bounds.merge(&local.transformed(&obj.transform.matrix()));
        }
        if bounds.is_valid() {
            Some(bounds)
        } else {
            None
        }
    }

    /// 整个场景（包含所有可见物体）的包围盒。
    pub fn bounds(&self, renderer: &Renderer) -> Option<quench3d_math::Aabb> {
        let mut bounds = quench3d_math::Aabb::EMPTY;
        for obj in &self.objects {
            if let Some((_, _, local)) = renderer.mesh_info(obj.mesh) {
                bounds.merge(&local.transformed(&obj.transform.matrix()));
            }
        }
        if bounds.is_valid() {
            Some(bounds)
        } else {
            None
        }
    }
}

/// 便捷构造器：用一组预设网格/材质快速搭建场景。
///
/// 内部缓存了常用图元与纯色材质，避免重复上传。
pub struct AssetCache {
    meshes: HashMap<String, MeshId>,
    materials: HashMap<String, MaterialId>,
    /// 单位立方体（边长为 1，中心在原点）。
    pub unit_cube: MeshId,
    /// 单位球（半径 0.5）。
    pub unit_sphere: MeshId,
    /// 单位圆柱（半径 0.5，高 1）。
    pub unit_cylinder: MeshId,
    /// 单位胶囊（半径 0.5，总高 2）。
    pub unit_capsule: MeshId,
    /// 单位平面（XZ 平面，边长 1）。
    pub unit_plane: MeshId,
}

impl AssetCache {
    /// 用常用图元初始化。
    pub fn new(renderer: &mut Renderer, ctx: &quench3d_render::GpuContext) -> Self {
        let unit_cube = renderer.add_mesh_named(ctx, &quench3d_render::MeshData::cube(1.0), "cube");
        let unit_sphere = renderer.add_mesh_named(
            ctx,
            &quench3d_render::MeshData::sphere(0.5, 32, 20),
            "sphere",
        );
        let unit_cylinder = renderer.add_mesh_named(
            ctx,
            &quench3d_render::MeshData::cylinder(0.5, 0.5, 28),
            "cylinder",
        );
        let unit_capsule = renderer.add_mesh_named(
            ctx,
            &quench3d_render::MeshData::capsule(0.5, 0.5, 28, 20),
            "capsule",
        );
        let unit_plane =
            renderer.add_mesh_named(ctx, &quench3d_render::MeshData::plane(1.0, 1), "plane");
        Self {
            meshes: HashMap::new(),
            materials: HashMap::new(),
            unit_cube,
            unit_sphere,
            unit_cylinder,
            unit_capsule,
            unit_plane,
        }
    }

    /// 上传一个命名网格（同名复用）。
    pub fn mesh(
        &mut self,
        renderer: &mut Renderer,
        ctx: &quench3d_render::GpuContext,
        name: &str,
        data: &quench3d_render::MeshData,
    ) -> MeshId {
        if let Some(id) = self.meshes.get(name) {
            return *id;
        }
        let id = renderer.add_mesh_named(ctx, data, name);
        self.meshes.insert(name.to_string(), id);
        id
    }

    /// 创建纯色 PBR 材质（相同参数复用）。
    ///
    /// `color` 按 **sRGB**（也就是「屏幕上看得到的颜色」）解释，
    /// 例如 `Vec4::new(0.85, 0.25, 0.20, 1.0)` 是砖红色。
    pub fn solid_material(
        &mut self,
        renderer: &mut Renderer,
        ctx: &quench3d_render::GpuContext,
        color: Vec4,
        metallic: f32,
        roughness: f32,
    ) -> MaterialId {
        let linear = Vec4::new(
            quench3d_render::color::srgb_to_linear(color.x),
            quench3d_render::color::srgb_to_linear(color.y),
            quench3d_render::color::srgb_to_linear(color.z),
            color.w,
        );
        self.solid_material_linear(renderer, ctx, linear, metallic, roughness)
    }

    /// 创建纯色 PBR 材质，`color` 为**线性**空间数值。
    pub fn solid_material_linear(
        &mut self,
        renderer: &mut Renderer,
        ctx: &quench3d_render::GpuContext,
        color: Vec4,
        metallic: f32,
        roughness: f32,
    ) -> MaterialId {
        let key = format!(
            "solid:{:.4},{:.4},{:.4},{:.4}|{:.2}|{:.2}",
            color.x, color.y, color.z, color.w, metallic, roughness
        );
        if let Some(id) = self.materials.get(&key) {
            return *id;
        }
        let desc = MaterialDesc::new(color).pbr(metallic, roughness);
        let id = renderer.add_material_named(ctx, desc, &key);
        self.materials.insert(key, id);
        id
    }

    /// 注册一个自定义材质（带贴图等）。
    pub fn material(
        &mut self,
        renderer: &mut Renderer,
        ctx: &quench3d_render::GpuContext,
        name: &str,
        desc: MaterialDesc,
    ) -> MaterialId {
        if let Some(id) = self.materials.get(name) {
            return *id;
        }
        let id = renderer.add_material_named(ctx, desc, name);
        self.materials.insert(name.to_string(), id);
        id
    }

    /// 缓存统计。
    pub fn cached_counts(&self) -> (usize, usize) {
        (self.meshes.len(), self.materials.len())
    }
}

/// 由物体缩放推断常用物理形状的辅助函数。
pub fn box_half_extents(scale: Vec3) -> Vec3 {
    scale.abs() * 0.5
}
