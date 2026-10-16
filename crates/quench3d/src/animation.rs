//! 关键帧动画的采样。
//!
//! 这个模块是**纯计算**：给定关键帧与时间算出姿态，不碰 GPU、不碰物理世界，
//! 所以能在没有显卡的环境里完整单测（CI 的三条系统腿都跑得到）。真正"把姿态写进
//! 世界"的那一步在 CLI 的脚本层（`Script::apply_animations`）。
//!
//! 语义（都是刻意的选择，不是实现细节）：
//! * 段内位置线性插值，朝向用四元数 slerp（不会绕远路）；
//! * `start` 之前保持第一帧；不循环时末尾截断在最后一帧；
//! * 循环按**轨道时长**取模（`speed` 在取模之前生效，所以倍速不会改变循环长度）。

use quench3d_math::{Quat, Vec3};

/// 一条关键帧轨道（位置：米；朝向：欧拉角 XYZ 度）。
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    times: Vec<f32>,
    values: Vec<Vec3>,
}

impl Track {
    /// 从"时间（秒）+ 取值"建一条轨道。
    ///
    /// 时间必须与取值等长、非空、非负、严格递增——这些错误在场景校验里已经拦过一次，
    /// 这里再拦一次是因为本模块也可能被别的调用方直接使用。
    pub fn new(times: Vec<f32>, values: Vec<Vec3>) -> Result<Self, String> {
        if times.len() != values.len() {
            return Err(format!(
                "关键帧的 times({}) 与 values({}) 长度不一致",
                times.len(),
                values.len()
            ));
        }
        if times.is_empty() {
            return Err("关键帧不能是空的".to_string());
        }
        let mut prev = f32::NEG_INFINITY;
        for t in &times {
            if !t.is_finite() || *t < 0.0 {
                return Err("关键帧时间必须是非负有限值".to_string());
            }
            if *t <= prev {
                return Err("关键帧时间必须严格递增".to_string());
            }
            prev = *t;
        }
        if values
            .iter()
            .any(|v| v.to_array().iter().any(|c| !c.is_finite()))
        {
            return Err("关键帧取值必须是有限数".to_string());
        }
        Ok(Self { times, values })
    }

    /// 最后一帧的时间（秒）；只有一帧时为 0。
    pub fn duration(&self) -> f32 {
        self.times.last().copied().unwrap_or(0.0)
    }

    /// 关键帧数量。
    pub fn len(&self) -> usize {
        self.times.len()
    }

    /// 是否没有关键帧（`new` 不会产生这种轨道，留给调用方判断用）。
    pub fn is_empty(&self) -> bool {
        self.times.is_empty()
    }

    /// 定位到"第几段 + 段内参数"，供位置与朝向两条路径共用。
    fn locate(&self, t: f32, looping: bool) -> (usize, f32) {
        if self.times.len() == 1 {
            return (0, 0.0);
        }
        let last = self.duration();
        let u = if looping && last > 0.0 {
            t.rem_euclid(last)
        } else {
            t.clamp(self.times[0], last)
        };
        let mut i = 0;
        while i + 2 < self.times.len() && u >= self.times[i + 1] {
            i += 1;
        }
        let (t0, t1) = (self.times[i], self.times[i + 1]);
        let f = if t1 > t0 {
            ((u - t0) / (t1 - t0)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        (i, f)
    }

    /// 采样：`looping` 为真时按轨道时长取模，否则两端截断。
    pub fn sample(&self, t: f32, looping: bool) -> Vec3 {
        let (i, f) = self.locate(t, looping);
        if i + 1 >= self.values.len() {
            return self.values[i];
        }
        self.values[i].lerp(self.values[i + 1], f)
    }
}

/// 朝向轨道：关键帧存欧拉角 XYZ（度），采样时转四元数做 slerp。
#[derive(Debug, Clone, PartialEq)]
pub struct RotationTrack {
    track: Track,
}

impl RotationTrack {
    /// 从"时间（秒）+ 欧拉角 XYZ（度）"建一条轨道。
    pub fn new(times: Vec<f32>, euler_deg: Vec<Vec3>) -> Result<Self, String> {
        Ok(Self {
            track: Track::new(times, euler_deg)?,
        })
    }

    /// 轨道时长（秒）：最后一帧的时间。
    pub fn duration(&self) -> f32 {
        self.track.duration()
    }

    /// 采样。段内用 slerp：从 0° 转到 180° 时不会出现"先绕到别处"的中间姿态。
    pub fn sample(&self, t: f32, looping: bool) -> Quat {
        let (i, f) = self.track.locate(t, looping);
        let a = euler_deg_to_quat(self.track.values[i]);
        if i + 1 >= self.track.values.len() {
            return a;
        }
        let b = euler_deg_to_quat(self.track.values[i + 1]);
        a.slerp(b, f)
    }
}

/// 位置混合：`w = 0` 取 `from`，`w = 1` 取 `to`，中间线性（`w` 会被夹到 \[0,1\]）。
pub fn blend_position(from: Vec3, to: Vec3, w: f32) -> Vec3 {
    from.lerp(to, w.clamp(0.0, 1.0))
}

/// 朝向混合：`w = 0` 取 `from`，`w = 1` 取 `to`，中间走 slerp（最短弧）。
pub fn blend_rotation(from: Quat, to: Quat, w: f32) -> Quat {
    from.slerp(to, w.clamp(0.0, 1.0))
}

/// 两帧之间"等效的"角速度（弧度/秒）：`dq = to * from⁻¹` 取轴角再除以 `dt`。
///
/// 为什么要它：动画每步直接设姿态，如果不动角速度，物理世界看到的就是"瞬移的姿态"——
/// 接触点没有切向速度，摩擦/滚动全都失效（球被动画转起来却不会滚）。补上角速度之后，
/// 摩擦才拿得到正确的切向速度。
///
/// 取**最短弧**：四元数 `q` 与 `-q` 表示同一姿态，所以先按 `w ≥ 0` 归一符号；
/// 否则从 350° 转到 0° 会被理解成"再转 350°"，而不是"回退 10°"。
pub fn angular_velocity_between(from: Quat, to: Quat, dt: f32) -> Vec3 {
    if dt <= 0.0 || !dt.is_finite() {
        return Vec3::ZERO;
    }
    let rel = to * from.inverse();
    let rel = if rel.w < 0.0 {
        Quat::new(-rel.x, -rel.y, -rel.z, -rel.w)
    } else {
        rel
    };
    let (axis, angle) = rel.to_axis_angle();
    axis * (angle / dt)
}

/// 欧拉角 XYZ（度）→ 四元数。
fn euler_deg_to_quat(v: Vec3) -> Quat {
    Quat::from_euler_xyz(v.x.to_radians(), v.y.to_radians(), v.z.to_radians())
}

/// 一条动画：按时间推着某个物体走（**运动学**，不是施加力）。
#[derive(Debug, Clone, PartialEq)]
pub struct Animation {
    /// 诊断用名字（场景里唯一，但不是查找键）。
    pub name: Option<String>,
    /// 目标刚体名；解析成句柄是调用方的事。
    pub target: String,
    /// 从第几秒开始。
    pub start: f32,
    /// 播放倍速（> 0）。
    pub speed: f32,
    /// 是否循环。
    pub looping: bool,
    /// 位置轨道（米）。
    pub position: Option<Track>,
    /// 朝向轨道（欧拉角 XYZ 度）。
    pub rotation: Option<RotationTrack>,
}

impl Animation {
    /// 建一条动画。参数含义见字段文档。
    pub fn new(
        name: Option<String>,
        target: String,
        start: f32,
        speed: f32,
        looping: bool,
        position: Option<Track>,
        rotation: Option<RotationTrack>,
    ) -> Result<Self, String> {
        if target.trim().is_empty() {
            return Err("动画目标名不能为空".to_string());
        }
        if !(speed.is_finite() && speed > 0.0) {
            return Err("speed 必须是正的有限值".to_string());
        }
        if !start.is_finite() {
            return Err("start 必须是有限值".to_string());
        }
        if position.is_none() && rotation.is_none() {
            return Err("动画至少要有一条 position 或 rotation 轨道".to_string());
        }
        Ok(Self {
            name,
            target,
            start,
            speed,
            looping,
            position,
            rotation,
        })
    }

    /// 轨道时长（秒）：取两条轨道里更长的那条。
    pub fn duration(&self) -> f32 {
        let p = self.position.as_ref().map(|t| t.duration()).unwrap_or(0.0);
        let r = self.rotation.as_ref().map(|t| t.duration()).unwrap_or(0.0);
        p.max(r)
    }

    /// 采样：返回 `(位置, 朝向)`；没给的轨道返回 `None`（调用方保持原值）。
    ///
    /// `world_time` 是秒。`start` 之前保持第一帧——"还没开始"不该等于"瞬移到别处"。
    pub fn sample(&self, world_time: f32) -> (Option<Vec3>, Option<Quat>) {
        let rel = (world_time - self.start) * self.speed;
        let looping = self.looping;
        let sample_at = |t: f32| if rel < 0.0 { 0.0 } else { t };
        let pos = self
            .position
            .as_ref()
            .map(|tr| tr.sample(sample_at(rel), looping));
        let rot = self
            .rotation
            .as_ref()
            .map(|tr| tr.sample(sample_at(rel), looping));
        (pos, rot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: f32, y: f32, z: f32) -> Vec3 {
        Vec3::new(x, y, z)
    }

    #[test]
    fn track_interpolates_clamps_and_needs_consistent_keys() {
        let tr = Track::new(
            vec![0.0, 1.0, 2.0],
            vec![v(0.0, 0.0, 0.0), v(2.0, 0.0, 0.0), v(2.0, 4.0, 0.0)],
        )
        .unwrap();
        assert_eq!(tr.duration(), 2.0);
        assert_eq!(tr.sample(0.5, false), v(1.0, 0.0, 0.0), "段内线性插值");
        assert_eq!(tr.sample(1.5, false), v(2.0, 2.0, 0.0), "第二段");
        assert_eq!(tr.sample(-5.0, false), v(0.0, 0.0, 0.0), "两端截断");
        assert_eq!(tr.sample(99.0, false), v(2.0, 4.0, 0.0), "末尾停在最后一帧");

        assert!(Track::new(vec![0.0], vec![]).is_err(), "长度不一致要报错");
        assert!(Track::new(vec![], vec![]).is_err(), "空轨道要报错");
        assert!(
            Track::new(vec![0.0, 0.0], vec![v(0.0, 0.0, 0.0); 2]).is_err(),
            "时间必须严格递增"
        );
        assert!(
            Track::new(vec![-1.0], vec![v(0.0, 0.0, 0.0)]).is_err(),
            "时间不能为负"
        );
    }

    #[test]
    fn track_loops_by_duration() {
        let tr = Track::new(vec![0.0, 1.0], vec![v(0.0, 0.0, 0.0), v(10.0, 0.0, 0.0)]).unwrap();
        assert_eq!(
            tr.sample(1.25, true),
            tr.sample(0.25, true),
            "循环：按 1 秒取模"
        );
        assert_eq!(tr.sample(2.5, true), tr.sample(0.5, true), "多圈也一样");
        assert_eq!(tr.sample(0.0, true), v(0.0, 0.0, 0.0));
    }

    #[test]
    fn rotation_track_slerps_between_euler_keys() {
        let rt =
            RotationTrack::new(vec![0.0, 1.0], vec![v(0.0, 0.0, 0.0), v(0.0, 90.0, 0.0)]).unwrap();
        let mid = rt.sample(0.5, false);
        let want = euler_deg_to_quat(v(0.0, 45.0, 0.0));
        assert!(
            mid.dot(want).abs() > 0.9999,
            "半程应当约等于 45°：{mid:?} vs {want:?}"
        );
        let end = rt.sample(1.0, false);
        assert!(end.dot(euler_deg_to_quat(v(0.0, 90.0, 0.0))).abs() > 0.9999);
        // 单帧轨道：一直是那个姿态
        let one = RotationTrack::new(vec![0.0], vec![v(10.0, 0.0, 0.0)]).unwrap();
        assert!(
            one.sample(5.0, true)
                .dot(euler_deg_to_quat(v(10.0, 0.0, 0.0)))
                .abs()
                > 0.9999
        );
    }

    #[test]
    fn angular_velocity_is_the_shortest_arc_divided_by_dt() {
        let q = |deg: f32| euler_deg_to_quat(v(0.0, deg, 0.0));
        // 90° / 0.1 秒 = π/2/0.1 ≈ 15.708 弧度/秒，绕 +Y
        let w = angular_velocity_between(q(0.0), q(90.0), 0.1);
        assert!(
            (w.y - std::f32::consts::FRAC_PI_2 / 0.1).abs() < 1e-3,
            "{w:?}"
        );
        assert!(w.x.abs() < 1e-4 && w.z.abs() < 1e-4, "只该有 Y 分量：{w:?}");

        // 最短弧：0° 到 350° 是"回退 10°"，不是"再往前转 350°"
        let back = angular_velocity_between(q(0.0), q(350.0), 1.0);
        assert!(back.y < 0.0, "应当往负方向回退：{back:?}");
        assert!((back.y + 10.0f32.to_radians()).abs() < 1e-3, "{back:?}");
        // 反过来 350° → 0° 就是继续往前 10°（这也是最短弧）
        let fwd = angular_velocity_between(q(350.0), q(0.0), 1.0);
        assert!(fwd.y > 0.0, "{fwd:?}");
        assert!((fwd.y - 10.0f32.to_radians()).abs() < 1e-3, "{fwd:?}");

        // 姿态没变 / dt 非法 → 零
        assert_eq!(angular_velocity_between(q(30.0), q(30.0), 0.5), Vec3::ZERO);
        assert_eq!(angular_velocity_between(q(0.0), q(90.0), 0.0), Vec3::ZERO);
        assert_eq!(
            angular_velocity_between(q(0.0), q(90.0), f32::NAN),
            Vec3::ZERO
        );
    }

    #[test]
    fn blending_hits_both_ends_and_the_midpoint() {
        let a = v(0.0, 1.0, 0.0);
        let b = v(0.0, 5.0, 0.0);
        assert_eq!(blend_position(a, b, 0.0), a, "权重 0 = 起点");
        assert_eq!(blend_position(a, b, 1.0), b, "权重 1 = 终点");
        assert_eq!(
            blend_position(a, b, 0.5),
            v(0.0, 3.0, 0.0),
            "权重 0.5 = 中点"
        );
        assert_eq!(blend_position(a, b, -1.0), a, "权重夹到 [0,1]");
        assert_eq!(blend_position(a, b, 2.0), b, "权重夹到 [0,1]");

        let qa = euler_deg_to_quat(v(0.0, 0.0, 0.0));
        let qb = euler_deg_to_quat(v(0.0, 0.0, 90.0));
        assert!(blend_rotation(qa, qb, 0.0).dot(qa).abs() > 0.9999);
        assert!(blend_rotation(qa, qb, 1.0).dot(qb).abs() > 0.9999);
        let mid = blend_rotation(qa, qb, 0.5);
        assert!(
            mid.dot(euler_deg_to_quat(v(0.0, 0.0, 45.0))).abs() > 0.9999,
            "半程是 45°：{mid:?}"
        );
    }

    #[test]
    fn rotation_segments_cannot_exceed_half_a_turn() {
        // 0° 与 360° 是同一个姿态：slerp 出来是一条常量，动画看起来"完全没动"。
        // 这不是实现缺陷，而是"关键帧存姿态"的固有语义；场景校验会给警告，
        // 文档也写明：要转更多圈就多插几个关键帧（每段 ≤ 180°）。
        let rt =
            RotationTrack::new(vec![0.0, 1.0], vec![v(0.0, 0.0, 0.0), v(0.0, 0.0, 360.0)]).unwrap();
        let q = rt.sample(0.5, false);
        assert!(q.dot(Quat::IDENTITY).abs() > 0.9999, "半程仍是恒等：{q:?}");
        // 180° 是可表达的极限；但恰好 180° 时"往哪边转"是歧义的（q 与 -q 是同一姿态），
        // 所以中点可能是 +90° 也可能是 -90°——两种解读都合法，测试两种都接受。
        let half =
            RotationTrack::new(vec![0.0, 1.0], vec![v(0.0, 0.0, 0.0), v(0.0, 0.0, 180.0)]).unwrap();
        let mid = half.sample(0.5, false);
        let plus = mid.dot(euler_deg_to_quat(v(0.0, 0.0, 90.0))).abs();
        let minus = mid.dot(euler_deg_to_quat(v(0.0, 0.0, -90.0))).abs();
        assert!(
            plus > 0.9999 || minus > 0.9999,
            "180° 段的中点应当是 ±90°：{mid:?}"
        );
        // 170° 段没有歧义：中点是 85°
        let lt =
            RotationTrack::new(vec![0.0, 1.0], vec![v(0.0, 0.0, 0.0), v(0.0, 0.0, 170.0)]).unwrap();
        let mid = lt.sample(0.5, false);
        assert!(
            mid.dot(euler_deg_to_quat(v(0.0, 0.0, 85.0))).abs() > 0.9999,
            "170° 段的中点是 85°：{mid:?}"
        );
    }

    #[test]
    fn animation_holds_before_start_and_speed_scales_time() {
        let pos = Track::new(vec![0.0, 1.0], vec![v(0.0, 0.0, 0.0), v(0.0, 4.0, 0.0)]).unwrap();
        let a = Animation::new(
            Some("跳".into()),
            "box".into(),
            2.0,
            1.0,
            false,
            Some(pos.clone()),
            None,
        )
        .unwrap();
        assert_eq!(
            a.sample(1.0).0,
            Some(v(0.0, 0.0, 0.0)),
            "start 之前保持第一帧"
        );
        assert_eq!(
            a.sample(2.5).0,
            Some(v(0.0, 2.0, 0.0)),
            "start 之后按 (t-start) 采样"
        );
        assert_eq!(a.duration(), 1.0);

        let fast = Animation::new(None, "box".into(), 0.0, 2.0, false, Some(pos), None).unwrap();
        assert_eq!(
            fast.sample(0.25).0,
            Some(v(0.0, 2.0, 0.0)),
            "倍速 2：0.25 秒走到 0.5 秒处"
        );
        assert_eq!(
            fast.sample(1.0).0,
            Some(v(0.0, 4.0, 0.0)),
            "播完停在最后一帧"
        );

        assert!(Animation::new(None, " ".into(), 0.0, 1.0, false, None, None).is_err());
        assert!(
            Animation::new(None, "box".into(), 0.0, 0.0, false, None, None).is_err(),
            "speed 必须 > 0"
        );
        assert!(
            Animation::new(None, "box".into(), 0.0, 1.0, false, None, None).is_err(),
            "至少要一条轨道"
        );
    }
}
