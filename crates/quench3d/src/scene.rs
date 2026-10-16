//! 场景：把"网格 + 材质 + 变换 + 可选物理刚体"组织成一个可提交给渲染器的列表。
//!
//! 这是引擎里最贴近"搭场景"的抽象：加一个物体、挂一个刚体、
//! 每帧自动从物理世界同步位姿，然后一次性提交绘制。
//!
//! 物体可以有**父物体**（[`SceneObject::parent`]），于是 `transform` 是**局部**变换，
//! 世界变换由 [`Scene::world_transform`] 沿父链现算 —— 拖动父物体时子物体自动跟着走。
//! 三条边界写死在这里：子物体只继承位置与朝向、**不继承缩放**（[`rigid_only`] 解释了为什么）；
//! 带刚体的物体不能有父物体（位姿归物理管）；删除物体要调 [`Scene::remove`]
//! （它负责把子孙提到祖父、保持它们的世界位姿、并把后面的父下标前移）。

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
    /// **局部**变换：有父物体时是相对父物体的，没有父物体时就是世界变换。
    ///
    /// 世界变换用 [`Scene::world_transform`] 现算（沿父链把局部变换拼上去），
    /// 所以移动父物体时子物体会自动跟着走，不需要任何"同步"步骤。
    pub transform: Transform,
    /// 父物体下标（`None` = 根）。
    ///
    /// 用下标而不是名字：场景里的物体本来就是按列表存取的，名字可能重复；
    /// 名字到下标的那层解析留在场景文件（`parent` 字段）与编辑器里。
    ///
    /// 子物体继承父物体的**位置与朝向**，但**不继承缩放**（原因见 [`rigid_only`]）。
    pub parent: Option<usize>,
    /// 关联的物理刚体（`None` 表示纯静态显示）。
    ///
    /// **带刚体的物体必须是根**（[`Scene::set_parent`] 会拒绝，场景校验器也会报错）：
    /// 刚体的位姿由物理决定，再挂一个父物体只会让父物体的运动被静默忽略。
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
    /// 生成绘制描述（`world` 是它的**世界**变换，见 [`Scene::world_transform`]）。
    pub fn instance(&self, world: Transform) -> InstanceDesc {
        let mut d = InstanceDesc::new(world).with_color(self.color);
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
            parent: None,
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
            // 带刚体的物体必然是根（`set_parent` 拒绝给带刚体的物体挂父物体），
            // 所以它的局部变换就是世界变换，直接写没问题。
            debug_assert!(
                obj.body.is_none() || obj.parent.is_none(),
                "带刚体的物体 \"{}\" 不该有父物体",
                obj.name
            );
            if obj.parent.is_some() {
                continue;
            }
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
        for (i, obj) in self.objects.iter().enumerate() {
            if let Some(handle) = obj.body {
                // 用世界变换：带刚体的物体是根，这里等价于 `obj.transform`，
                // 但写成世界变换后，"以后允许给刚体挂父物体"不会变成一个静默的错。
                if let Some(body) = world.body_mut(handle) {
                    body.set_transform(self.world_transform(i));
                }
            }
        }
    }

    /// 提交全部可见物体到渲染器。
    pub fn submit(&self, renderer: &mut Renderer) {
        for (i, obj) in self.objects.iter().enumerate() {
            if obj.visible {
                renderer.draw(
                    obj.mesh,
                    obj.material,
                    obj.instance(self.world_transform(i)),
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // 层级（父子）
    // -----------------------------------------------------------------------

    /// 物体的**世界**变换：沿父链把局部变换逐级拼上去。
    ///
    /// **只继承位置与朝向，不继承缩放**（见 [`rigid_only`]）。父链断掉（下标越界）或
    /// 成环时立刻停下返回当前结果 —— 宁给一个能用的值，也不要在这里死循环
    /// （界面每帧都会调它）。
    pub fn world_transform(&self, index: usize) -> Transform {
        let Some(obj) = self.objects.get(index) else {
            return Transform::IDENTITY;
        };
        let mut world = obj.transform;
        let mut cursor = obj.parent;
        let mut steps = 0usize;
        while let Some(p) = cursor {
            let Some(parent) = self.objects.get(p) else {
                break; // 断链：按当前结果返回（校验器会先一步报错）
            };
            world = rigid_only(&parent.transform).mul_transform(&world);
            cursor = parent.parent;
            steps += 1;
            if steps > self.objects.len() {
                break; // 成环：不转圈
            }
        }
        world
    }

    /// 把世界变换写回物体（层级里存的是局部变换，这里做一次换算）。
    ///
    /// 返回 `false` 表示下标越界，什么都没改。
    pub fn set_world_transform(&mut self, index: usize, world: Transform) -> bool {
        let Some(obj) = self.objects.get(index) else {
            return false;
        };
        let local = match obj.parent {
            None => world,
            Some(p) => rigid_only(&self.world_transform(p)).child_of(&world),
        };
        self.objects[index].transform = local;
        true
    }

    /// 设置父物体（`None` = 变成根）；成功时**不改动**物体的世界位姿之外的东西。
    ///
    /// 注意：调用方通常想"保持世界位姿不变"，那需要自己先取 [`Scene::world_transform`]、
    /// 设完父物体后调 [`Scene::set_world_transform`] 写回去。
    ///
    /// 拒绝三种情况：自身、成环、以及"给带刚体的物体挂父物体"
    /// （刚体位姿由物理决定，父物体只会被静默忽略 —— 这种"看着像生效了"的错误最难查）。
    pub fn set_parent(&mut self, child: usize, parent: Option<usize>) -> Result<(), String> {
        if child >= self.objects.len() {
            return Err(format!("物体下标 {child} 越界"));
        }
        if let Some(p) = parent {
            if p >= self.objects.len() {
                return Err(format!("父物体下标 {p} 越界"));
            }
            if p == child {
                return Err("物体不能以自己为父".to_string());
            }
            if self.is_ancestor(child, p) {
                return Err(format!(
                    "父子关系会成环：\"{}\" 已经是 \"{}\" 的祖先",
                    self.objects[child].name, self.objects[p].name
                ));
            }
            if self.objects[child].body.is_some() {
                return Err(format!(
                    "\"{}\" 带刚体，位姿由物理决定，不能挂到父物体下",
                    self.objects[child].name
                ));
            }
        }
        self.objects[child].parent = parent;
        Ok(())
    }

    /// `ancestor` 是不是 `node` 的祖先（含间接；不含自己）。
    pub fn is_ancestor(&self, ancestor: usize, node: usize) -> bool {
        let mut cursor = self.objects.get(node).and_then(|o| o.parent);
        let mut steps = 0usize;
        while let Some(p) = cursor {
            if p == ancestor {
                return true;
            }
            cursor = self.objects.get(p).and_then(|o| o.parent);
            steps += 1;
            if steps > self.objects.len() {
                break;
            }
        }
        false
    }

    /// 直接子物体（按场景顺序）。
    pub fn children_of(&self, parent: usize) -> Vec<usize> {
        self.objects
            .iter()
            .enumerate()
            .filter(|(_, o)| o.parent == Some(parent))
            .map(|(i, _)| i)
            .collect()
    }

    /// 删除一个物体：返回被删的物体。
    ///
    /// 直接子物体**提到它的父级**（删父不连带删子），并且**保持世界位姿不变** ——
    /// 只挪父下标的话，子物体会"跳"到祖父坐标系里去（父物体有位移时尤其明显）。
    /// 其余物体排在删除位置之后的，父下标也会跟着整体前移 —— 这一步必须由场景自己做，
    /// 调用方手工 `objects.remove(i)` 会让所有排在后面的父子关系全部错位。
    pub fn remove(&mut self, index: usize) -> Option<SceneObject> {
        if index >= self.objects.len() {
            return None;
        }
        // 删之前先记下子物体的世界变换（删完父链就断了，算不出来了）。
        let promoted: Vec<(usize, Transform)> = self
            .children_of(index)
            .into_iter()
            .map(|child| (child, self.world_transform(child)))
            .collect();

        let removed = self.objects.remove(index);
        let grandparent = removed.parent.map(|g| if g > index { g - 1 } else { g });
        for obj in &mut self.objects {
            match obj.parent {
                Some(p) if p == index => obj.parent = grandparent,
                Some(p) if p > index => obj.parent = Some(p - 1),
                _ => {}
            }
        }
        // 再把世界变换写回去（`set_world_transform` 会按**新的**父物体换算成局部量）。
        for (child, world) in promoted {
            let shifted = if child > index { child - 1 } else { child };
            self.set_world_transform(shifted, world);
        }
        Some(removed)
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
            bounds.merge(&local.transformed(&self.world_transform(i).matrix()));
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
        for (i, obj) in self.objects.iter().enumerate() {
            if let Some((_, _, local)) = renderer.mesh_info(obj.mesh) {
                bounds.merge(&local.transformed(&self.world_transform(i).matrix()));
            }
        }
        if bounds.is_valid() {
            Some(bounds)
        } else {
            None
        }
    }
}

/// 取一个变换的"刚性部分"：位置 + 朝向，**缩放按 1 算**。
///
/// 场景层级刻意**不继承缩放**。这个引擎里物体的 `scale` 就是图元的尺寸
/// （场景文件的 `shape.size` / `radius` 会被折算成它）：如果按标准 TRS
/// 乘法继承，把一棵树挂到 `size: [20, 0.2, 20]` 的地面下，树会立刻被压成一块饼 ——
/// 而"挂到地板上"恰恰是最常见的用法。
///
/// 所以 `scale` 的含义是"**这个物体**自己多大"，不是"这一组多大"。
/// 想整体缩放一组物体，得逐个改（编辑器里也是这个语义）。
/// 组合与反解都走这一个函数，保证"世界→局部→世界"是精确往返。
fn rigid_only(t: &Transform) -> Transform {
    Transform::new(t.position, t.rotation, Vec3::ONE)
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

#[cfg(test)]
mod tests {
    use super::*;
    use quench3d_math::Quat;

    /// 纯逻辑测试：层级只碰 `Scene` 自己，不需要 GPU（网格/材质句柄就是 `u32`）。
    /// 编辑器那边也有一份带真实装载的集成测试，但那些在没显卡的机器上会跳过，
    /// 这一份保证"没显卡也要能验证层级"。
    fn scene_with_chain() -> Scene {
        let mut scene = Scene::new();
        let m = 1u32;
        // 父：位置 (10,0,0)、绕 Y 转 90°、**非均匀**缩放 (2,0.5,3)
        let p = scene.add_named(
            m,
            m,
            Transform::new(
                Vec3::new(10.0, 0.0, 0.0),
                Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
                Vec3::new(2.0, 0.5, 3.0),
            ),
            "父",
        );
        // 子：局部位置 (0,1,0)、自己的缩放 (1,2,1)
        let c = scene.add_named(
            m,
            m,
            Transform::new(
                Vec3::new(0.0, 1.0, 0.0),
                Quat::IDENTITY,
                Vec3::new(1.0, 2.0, 1.0),
            ),
            "子",
        );
        scene.set_parent(c, Some(p)).expect("挂父物体应当成功");
        assert_eq!(scene.find("父"), Some(p));
        scene
    }

    #[test]
    fn world_transform_inherits_position_and_rotation_but_not_scale() {
        let scene = scene_with_chain();
        let child = scene.find("子").unwrap();
        let world = scene.world_transform(child);
        // "父"绕 Y 转 90°：局部的 (0,1,0) 在世界里还是 (0,1,0)（Y 轴旋转不动 Y 轴）。
        assert!(
            (world.position - Vec3::new(10.0, 1.0, 0.0)).length() < 1e-5,
            "{:?}",
            world.position
        );
        // 缩放是子物体自己的：父物体的 (2,0.5,3) **不**乘进来。
        assert_eq!(world.scale, Vec3::new(1.0, 2.0, 1.0));
        // 朝向被子物体继承（父物体绕 Y 转了 90°）。比四元数而不是欧拉角：
        // 欧拉角反解在轴附近有 1e-5 量级的抖动，写 `== 90.0` 会在别的平台上红
        // （macOS 上就真红过一次：90.00001 != 90.0）。
        let expected = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
        assert!(
            world.rotation.dot(expected).abs() > 0.9999,
            "子物体没有继承父物体的朝向：{:?}",
            world.rotation
        );
        // 根物体的世界变换就是局部变换。
        let parent = scene.find("父").unwrap();
        assert_eq!(
            scene.world_transform(parent).scale,
            Vec3::new(2.0, 0.5, 3.0)
        );
    }

    #[test]
    fn moving_the_parent_moves_the_child() {
        let mut scene = scene_with_chain();
        let parent = scene.find("父").unwrap();
        let child = scene.find("子").unwrap();
        let before_local = scene.objects[child].transform;
        let mut world = scene.world_transform(parent);
        world.position += Vec3::new(0.0, 3.0, 0.0);
        assert!(scene.set_world_transform(parent, world));
        assert_eq!(
            scene.world_transform(child).position,
            Vec3::new(10.0, 4.0, 0.0)
        );
        assert_eq!(
            scene.objects[child].transform.position,
            before_local.position
        );
    }

    #[test]
    fn world_and_local_conversions_are_exact_inverses() {
        let mut scene = scene_with_chain();
        let child = scene.find("子").unwrap();
        let target = Transform::new(
            Vec3::new(-3.0, 2.5, 7.0),
            Quat::from_rotation_x(0.4),
            Vec3::new(0.5, 0.5, 0.5),
        );
        assert!(scene.set_world_transform(child, target));
        // 写回去的世界变换与给的一致（中间的局部量不需要人来算）。
        let back = scene.world_transform(child);
        assert!((back.position - target.position).length() < 1e-5);
        assert!((back.scale - target.scale).length() < 1e-5);
    }

    #[test]
    fn set_parent_rejects_cycles_and_bodies() {
        let mut scene = scene_with_chain();
        let parent = scene.find("父").unwrap();
        let child = scene.find("子").unwrap();
        // 自己
        assert!(scene.set_parent(parent, Some(parent)).is_err());
        // 成环：把父挂到自己的子下面
        assert!(scene.set_parent(parent, Some(child)).is_err());
        assert_eq!(scene.objects[parent].parent, None, "失败不能改坏层级");
        // 越界
        assert!(scene.set_parent(99, Some(parent)).is_err());
        assert!(scene.set_parent(child, Some(99)).is_err());
        // 带刚体的物体（用一个假句柄）不能挂父物体 —— 位姿归物理管
        let body = scene.add(1u32, 1u32, Transform::IDENTITY);
        scene.objects[body].body = Some(quench3d_physics::BodyHandle::new(0, 0));
        assert!(scene.set_parent(body, Some(parent)).is_err());
        // children_of / is_ancestor
        assert_eq!(scene.children_of(parent), vec![child]);
        assert!(scene.is_ancestor(parent, child));
        assert!(!scene.is_ancestor(child, parent));
    }

    #[test]
    fn remove_promotes_children_without_moving_them() {
        let mut scene = scene_with_chain();
        let parent = scene.find("父").unwrap();
        let child = scene.find("子").unwrap();
        let before = scene.world_transform(child);
        // 后面再加一个物体，验证下标前移
        let tail = scene.add(1, 1, Transform::IDENTITY);
        scene.set_parent(tail, Some(parent)).expect("挂父物体");

        let removed = scene.remove(parent).expect("父应当存在");
        assert_eq!(removed.name, "父");
        // 子物体成了根物体（父物体自己就是根），但世界位置没动。
        let child_now = scene.find("子").unwrap();
        assert_eq!(scene.objects[child_now].parent, None);
        assert!(
            (scene.world_transform(child_now).position - before.position).length() < 1e-5,
            "{:?} vs {:?}",
            scene.world_transform(child_now).position,
            before.position
        );
        // 排在后面的物体下标前移一位（`add` 的缺省名就是 "object"）。
        let tail_now = scene
            .find("object")
            .unwrap_or_else(|| panic!("尾物体应当还在，实际有 {}", scene.len()));
        assert!(tail_now < tail);
        assert_eq!(scene.len(), 2);
    }
}
