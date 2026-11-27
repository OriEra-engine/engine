//! L5 程序化天空(feature `sky`,默认关)——真实场景批次 WP6。
//!
//! 一个**线性 HDR 辐射场**:给视线方向,回该方向的可见辐射(色调映射
//! **之前**的线性值,不是显示色)。三个零件:天顶↔地平线梯度、硬边太阳
//! 盘、`overcast` 阴天压平;[`env_from_sky`] 再把场固定成方向环查询表,
//! 给后续 IBL 当参考 env。
//!
//! 契约(骨架立好的,填充时保持)
//! - 纯 CPU、headless 可测,零新增依赖:本文件只碰 `std`,不 `use` crate
//!   内任何其它模块(同批次的 `shadow` / `ibl` / `cull` / `instancing` /
//!   `scene` 一律不碰——天空喂 IBL 的接线是后续批次);
//! - 向量一律 `[f32; 3]`,坐标 **y 向上**:`dir.y > 0` 是天穹、`dir.y < 0`
//!   是地面;本模块用不到矩阵;
//! - 输出是预色调映射的**线性 HDR**:天穹典型 0~1,太阳盘几十——大于
//!   1.0 是常态不是 bug,压到显示范围是后续色调映射的事;
//! - 确定性:无随机、无哈希序依赖,同输入同机两次跑逐位一致。
//!
//! 诚实边界
//! - 这是**美术梯度天**,不是大气散射:没有 Rayleigh/Mie、没有多重散射、
//!   没有雾/航空透视;日落红晕靠 tint 调,太阳轨迹(何时何地落山)归上层;
//! - 太阳盘是几何硬边、盘内均匀(三通道同加 `sun_radiance`,中性白):
//!   没有临边昏暗、没有辉光——辉光归泛光/后处理;
//! - 盘的叠加**不区分半球**:太阳落山后直视地面方向仍见盘;被真实地形
//!   挡住是场景几何的事,本模块不管;
//! - 阴天只压天穹梯度、压灭太阳盘,不压地面 tint,也不产生云:全阴时
//!   地平线处会有"天亮地暗"的硬边;
//! - `f32::sin/cos` 走系统 libm:同机确定性没问题,跨平台最后一 ulp 可能
//!   漂;要跨平台逐位一致得换固定查表(后续批次)。
//!
//! 数值都是美术标定([`SkyParams::default`] 是起点),不是物理单位。

/// [`env_from_sky`] 每个纬度环的方位采样数(固定,行序与总行数因此确定)。
pub const ENV_AZIMUTHS: u32 = 8;

/// [`env_from_sky`] 的环数上限:参考表不是烘焙目标,夹掉 absurd 输入防止
/// 内存打爆(超过按 256 处理)。
pub const ENV_MAX_BANDS: u32 = 256;

/// 天空参数。所有辐射量都是预色调映射的线性 HDR 值。
///
/// 用 [`SkyParams::new`] 构造(归一化/夹取/校验都发生在构造时);字段是
/// pub 的,手写结构体字面量请自己守住下述不变量——求值端另有防御,破坏
/// 不变量只会退化(无盘/不压平),不会 NaN、不会外插。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkyParams {
    /// 指向太阳的**单位**方向(y 向上)。`new` 归一化;零/非有限向量在
    /// `new` 里 panic(构造期程序错误)。
    pub sun_dir: [f32; 3],
    /// 盘内叠加的辐射(三通道同值 = 中性白盘)。负/非有限按 0(无太阳)。
    pub sun_radiance: f32,
    /// 地平线(`dir.y == 0`)的辐射。应为有限值,非有限会如实传播。
    pub horizon_tint: [f32; 3],
    /// 天顶(`dir.y == 1`)的辐射。同上,不做校验。
    pub zenith_tint: [f32; 3],
    /// 下半球(`dir.y < 0`)的辐射——地面反照率的粗略代理,不受阴天影响。
    pub ground_tint: [f32; 3],
    /// 阴天系数 [0,1]:0 晴空、1 全阴(盘消失、天穹均匀亮)。`new` 夹取,
    /// 非有限按 0。
    pub overcast: f32,
    /// 太阳盘**角半径**(度;是半径不是直径,物理太阳 ≈0.26°)。0 无盘,
    /// 180 整球是盘(数学退化,别这么配)。`new` 夹到 [0,180]。
    pub sun_disc_deg: f32,
}

impl SkyParams {
    /// 全字段构造。`sun_dir` 归一化,零/非有限 panic(与 `Camera::look_at`
    /// 的构造期参数校验同一风格);`overcast` 夹到 [0,1]、`sun_disc_deg`
    /// 夹到 [0,180],两者的非有限值都按 0 处理;`sun_radiance` 负/非有限
    /// 按 0。tint 不校验(见字段文档)。
    pub fn new(
        sun_dir: [f32; 3],
        sun_radiance: f32,
        horizon_tint: [f32; 3],
        zenith_tint: [f32; 3],
        ground_tint: [f32; 3],
        overcast: f32,
        sun_disc_deg: f32,
    ) -> Self {
        let n =
            (sun_dir[0] * sun_dir[0] + sun_dir[1] * sun_dir[1] + sun_dir[2] * sun_dir[2]).sqrt();
        assert!(
            n.is_finite() && n > 0.0,
            "sun_dir 必须是非零有限向量, got {sun_dir:?}"
        );
        Self {
            sun_dir: [sun_dir[0] / n, sun_dir[1] / n, sun_dir[2] / n],
            sun_radiance: if sun_radiance.is_finite() {
                sun_radiance.max(0.0)
            } else {
                0.0
            },
            horizon_tint,
            zenith_tint,
            ground_tint,
            overcast: if overcast.is_finite() {
                overcast.clamp(0.0, 1.0)
            } else {
                0.0
            },
            sun_disc_deg: if sun_disc_deg.is_finite() {
                sun_disc_deg.clamp(0.0, 180.0)
            } else {
                0.0
            },
        }
    }
}

impl Default for SkyParams {
    /// 美术起点:晴天、太阳仰角 45°(朝 +z)、盘内峰值 40。数值非物理标定。
    fn default() -> Self {
        Self::new(
            [
                0.0,
                std::f32::consts::FRAC_1_SQRT_2,
                std::f32::consts::FRAC_1_SQRT_2,
            ],
            40.0,
            [0.60, 0.75, 0.95],
            [0.15, 0.30, 0.65],
            [0.22, 0.19, 0.15],
            0.0,
            0.5,
        )
    }
}

/// `[f32; 3]` 点积。
fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// 归一化到单位长度;零向量(含下溢到 0 的极端小向量)或非有限分量返回
/// `None`——调用方决定诚实退化(无辐射 / 无盘)。
fn unit3(v: [f32; 3]) -> Option<[f32; 3]> {
    let n = dot3(v, v).sqrt();
    if n.is_finite() && n > 0.0 {
        Some([v[0] / n, v[1] / n, v[2] / n])
    } else {
        None
    }
}

/// `a*(1-t) + b*t`:写成这个形状是为了 **t=0/1 时逐位落在端点**(先乘后
/// 加;`a + (b-a)*t` 的展开在端点会有舍入差,不能用)。
fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    let inv = 1.0 - t;
    [
        a[0] * inv + b[0] * t,
        a[1] * inv + b[1] * t,
        a[2] * inv + b[2] * t,
    ]
}

/// 两 tint 的中点(阴天天穹的均匀亮度)。
fn mid3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        0.5 * (a[0] + b[0]),
        0.5 * (a[1] + b[1]),
        0.5 * (a[2] + b[2]),
    ]
}

/// 方向 `dir` 上的天空辐射(线性 HDR,预色调映射)。
///
/// `dir` 内部归一化;**零向量(含下溢)或非有限分量返回 `[0,0,0]`**——
/// 没有方向就没有辐射,诚实结果,不 panic、不 NaN。
///
/// 公式(`y` = 归一化后 `dir.y`,`w` = `overcast`):
/// 1. 基础辐射:`y < 0` → `ground_tint`;`y ≥ 0` →
///    `horizon_tint*(1-y) + zenith_tint*y`(zenith↔horizon 按 `|dir.y|`
///    插值;`y=0/1` 逐位落在端点 tint;`y = ±0` 归地平线,落在
///    `horizon_tint` 上);
/// 2. 阴天压平(只作用天穹,地面不动):
///    `base ← base*(1-w) + flat*w`,其中
///    `flat = 0.5*(horizon_tint + zenith_tint)`;`w=0` 逐位不变,`w=1`
///    天穹逐位等于 `flat`;
/// 3. 太阳盘(叠加,不区分半球):`dot(dir, sun_dir) ≥ cos(sun_disc_deg)`
///    时 `out += sun_radiance*(1-w)`。**硬边**:恰好等于阈值算盘内,盘内
///    均匀无衰减;`w=1` 加零,盘消失。
///
/// `sun_dir` 求值时防御性再归一化:手写字面量破坏单位长度不会让盘变形,
/// 零/非有限等价"无太阳"。
pub fn sky_radiance(dir: [f32; 3], p: &SkyParams) -> [f32; 3] {
    let Some(d) = unit3(dir) else {
        return [0.0; 3];
    };
    let y = d[1];
    let mut out = if y < 0.0 {
        p.ground_tint
    } else {
        let grad = lerp3(p.horizon_tint, p.zenith_tint, y);
        if p.overcast > 0.0 {
            // `> 0` 门挡住负值与 NaN(手写字面量);min(1.0) 挡住 >1,
            // 不做外插。
            lerp3(
                grad,
                mid3(p.horizon_tint, p.zenith_tint),
                p.overcast.min(1.0),
            )
        } else {
            grad
        }
    };
    if let Some(s) = unit3(p.sun_dir) {
        let gain = disc_gain(d, s, p);
        if gain > 0.0 {
            out = [out[0] + gain, out[1] + gain, out[2] + gain];
        }
    }
    out
}

/// 太阳盘的叠加量(三通道同值)。`d`、`s` 都是已归一化方向。
///
/// 阈值判等含盘内(`dot == cos_thr` 也算);`sun_disc_deg` 非有限或 ≤0
/// (手写字面量)→ 无盘;>180 → 夹到 180(整球是盘,数学退化)。
fn disc_gain(d: [f32; 3], s: [f32; 3], p: &SkyParams) -> f32 {
    // NaN 一律视作"无盘"(is_finite 先挡,避免否定比较的歧义)。
    if !p.sun_radiance.is_finite() || p.sun_radiance <= 0.0 {
        return 0.0;
    }
    if !p.sun_disc_deg.is_finite() || p.sun_disc_deg <= 0.0 {
        return 0.0;
    }
    let cos_thr = p.sun_disc_deg.min(180.0).to_radians().cos();
    if dot3(d, s) >= cos_thr {
        p.sun_radiance * (1.0 - p.overcast)
    } else {
        0.0
    }
}

/// 固定方向环采样表:`(方向, 辐射)` 逐行列出,辐射即 [`sky_radiance`] 在
/// 该方向的值。给后续 IBL 当**参考 env** 的 CPU 查询表——不是烘焙目标。
///
/// 行序(固定,可逐位对拍):
/// - `bands` 个纬度环(超过 [`ENV_MAX_BANDS`] 按 256 处理;0 → 空表),
///   环 `i` 取**环中点**极角 `θ_i = π*(i+0.5)/bands`,从天顶(i=0)向天底
///   递增,`y = cosθ` 沿环序严格递减;
/// - 每环固定 [`ENV_AZIMUTHS`] 个方位角 `φ_j = 2π*j/8`(j 升序):`φ=0`
///   指向 +z,φ 增大先偏向 +x;
/// - 方向 = `[sinθ*sinφ, cosθ, sinθ*cosφ]`,单位长度到 ~1e-7(IBL 对拍
///   够用,不做逐行再归一化——除数随 φ 微变会破坏"环内 y 逐位一致");
/// - 总行数 = `min(bands, 256) * 8`,每行辐射与直接调 [`sky_radiance`]
///   逐位一致。
pub fn env_from_sky(p: &SkyParams, bands: u32) -> Vec<([f32; 3], [f32; 3])> {
    let bands = bands.min(ENV_MAX_BANDS);
    let mut table = Vec::with_capacity((bands * ENV_AZIMUTHS) as usize);
    for ring in 0..bands {
        let theta = std::f32::consts::PI * (ring as f32 + 0.5) / bands as f32;
        let sy = theta.sin();
        let dir_y = theta.cos();
        for j in 0..ENV_AZIMUTHS {
            let phi = std::f32::consts::TAU * j as f32 / ENV_AZIMUTHS as f32;
            // y 直接用每环同一个 cosθ 值(不做逐行再归一化),环内逐位一致。
            let dir = [sy * phi.sin(), dir_y, sy * phi.cos()];
            let radiance = sky_radiance(dir, p);
            table.push((dir, radiance));
        }
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 相对 + 绝对混合容差(端点逐位的断言不用它,用 assert_eq)。
    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() <= 1e-5 * a.abs().max(b.abs()).max(1.0)
    }

    fn close3(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b.iter()).all(|(x, y)| close(*x, *y))
    }

    /// 晴天参考:太阳仰角约 53°(朝 +x)、盘 0.5° 半径、峰值 25。
    fn clear() -> SkyParams {
        SkyParams::new(
            [0.6, 0.8, 0.0],
            25.0,
            [0.70, 0.80, 0.95],
            [0.15, 0.30, 0.60],
            [0.22, 0.18, 0.14],
            0.0,
            0.5,
        )
    }

    #[test]
    fn new_normalizes_sun_dir_and_clamps_scalars() {
        let p = SkyParams::new(
            [3.0, 4.0, 0.0],
            25.0,
            [0.0; 3],
            [0.0; 3],
            [0.0; 3],
            2.0,
            400.0,
        );
        let n = dot3(p.sun_dir, p.sun_dir).sqrt();
        assert!(close(n, 1.0), "sun_dir 应归一化: {n}");
        assert!(close3(p.sun_dir, [0.6, 0.8, 0.0]));
        assert_eq!(p.overcast, 1.0, "overcast 夹到 [0,1]");
        assert_eq!(p.sun_disc_deg, 180.0, "盘角夹到 [0,180]");
        let q = SkyParams::new(
            [0.0, 1.0, 0.0],
            1.0,
            [0.0; 3],
            [0.0; 3],
            [0.0; 3],
            -3.0,
            -1.0,
        );
        assert_eq!(q.overcast, 0.0);
        assert_eq!(q.sun_disc_deg, 0.0, "≤0 → 无盘");
    }

    #[test]
    #[should_panic(expected = "sun_dir")]
    fn new_rejects_zero_sun_dir() {
        SkyParams::new([0.0; 3], 1.0, [0.0; 3], [0.0; 3], [0.0; 3], 0.0, 1.0);
    }

    #[test]
    #[should_panic(expected = "sun_dir")]
    fn new_rejects_non_finite_sun_dir() {
        let _ = SkyParams::new(
            [f32::NAN, 1.0, 0.0],
            1.0,
            [0.0; 3],
            [0.0; 3],
            [0.0; 3],
            0.0,
            1.0,
        );
    }

    #[test]
    fn zenith_and_horizon_endpoints_are_bit_exact() {
        let p = clear();
        assert_eq!(
            sky_radiance([0.0, 1.0, 0.0], &p),
            p.zenith_tint,
            "天顶逐位 = zenith_tint"
        );
        for dir in [
            [1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, -1.0],
            [-1.0, 0.0, 0.0],
        ] {
            assert_eq!(
                sky_radiance(dir, &p),
                p.horizon_tint,
                "地平线逐位 = horizon_tint: {dir:?}"
            );
        }
        // y = -0.0 归天穹分支(IEEE 比较 -0.0 >= 0.0 成立),仍是 horizon_tint。
        assert_eq!(sky_radiance([-1.0, -0.0, 0.0], &p), p.horizon_tint);
    }

    #[test]
    fn lower_hemisphere_is_ground_tint_even_overcast() {
        let p = clear();
        let oc1 = SkyParams { overcast: 1.0, ..p };
        for dir in [[0.0, -1.0, 0.0], [0.6, -0.8, 0.0], [0.0, -0.3, -0.9]] {
            assert_eq!(sky_radiance(dir, &p), p.ground_tint, "下半球 = ground_tint");
            assert_eq!(
                sky_radiance(dir, &oc1),
                p.ground_tint,
                "阴天不压地面: {dir:?}"
            );
        }
    }

    #[test]
    fn sun_disc_is_brightest_spot_in_the_whole_dome() {
        let p = clear(); // 盘峰值 25,tint ≤ 0.95 → 盘内每通道必然全场最高
        let sun = sky_radiance(p.sun_dir, &p);
        let mut dirs: Vec<[f32; 3]> = env_from_sky(&p, 16).into_iter().map(|(d, _)| d).collect();
        dirs.extend([
            [0.0, 1.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, -1.0, 0.0],
        ]);
        for dir in &dirs {
            let r = sky_radiance(*dir, &p);
            for (s, r) in sun.iter().zip(r.iter()) {
                assert!(s >= r, "盘外方向超过太阳点: dir={dir:?} {} vs {}", r, s);
            }
        }
        assert!(
            sun[0] > sky_radiance([0.0, 1.0, 0.0], &p)[0],
            "盘能量让太阳点严格高于天顶"
        );
    }

    #[test]
    fn sun_disc_hard_edge_uniform_and_below_horizon() {
        // 太阳放天顶,盘角半径 5°:4° 在盘内、6° 在盘外(避开浮点边界)。
        let p = SkyParams::new(
            [0.0, 1.0, 0.0],
            25.0,
            [0.5, 0.6, 0.7],
            [0.1, 0.2, 0.3],
            [0.2, 0.2, 0.2],
            0.0,
            5.0,
        );
        let p0 = SkyParams {
            sun_disc_deg: 0.0,
            ..p
        };
        let inside = [4.0f32.to_radians().sin(), 4.0f32.to_radians().cos(), 0.0];
        let outside = [6.0f32.to_radians().sin(), 6.0f32.to_radians().cos(), 0.0];
        let hit = sky_radiance(inside, &p);
        let base_hit = sky_radiance(inside, &p0);
        let miss = sky_radiance(outside, &p);
        let base_miss = sky_radiance(outside, &p0);
        for ((h, b), (m, bm)) in hit
            .iter()
            .zip(base_hit.iter())
            .zip(miss.iter().zip(base_miss.iter()))
        {
            assert_eq!(*h, *b + 25.0, "盘内 = 基础 + sun_radiance(逐位)");
            assert_eq!(*m, *bm, "盘外逐位等于无盘结果");
        }
        // 盘叠加不区分半球:太阳在地平线下,直视地面方向仍见盘。
        let night = SkyParams::new(
            [0.0, -1.0, 0.0],
            25.0,
            [0.5, 0.6, 0.7],
            [0.1, 0.2, 0.3],
            [0.2, 0.2, 0.2],
            0.0,
            10.0,
        );
        let night0 = SkyParams {
            sun_disc_deg: 0.0,
            ..night
        };
        let down = sky_radiance([0.0, -1.0, 0.0], &night);
        let base = sky_radiance([0.0, -1.0, 0.0], &night0);
        for (h, b) in down.iter().zip(base.iter()) {
            assert_eq!(*h, *b + 25.0, "落山的盘在地平线下仍可见");
        }
    }

    #[test]
    fn overcast_flattens_gradient_and_erases_disc() {
        let p = clear();
        let flat = mid3(p.horizon_tint, p.zenith_tint);
        let full = SkyParams { overcast: 1.0, ..p };
        // w=1:整个天穹逐位 = flat,太阳盘消失。
        assert_eq!(sky_radiance(p.sun_dir, &full), flat, "太阳盘在 w=1 消失");
        assert_eq!(sky_radiance([0.0, 1.0, 0.0], &full), flat);
        assert_eq!(sky_radiance([0.9, 0.1, 0.0], &full), flat);
        // 半阴:天顶 = z*(1-w) + flat*w(容差比较);盘仍占优。
        let half = SkyParams { overcast: 0.5, ..p };
        let z = sky_radiance([0.0, 1.0, 0.0], &half);
        for (zv, (zt, ft)) in z.iter().zip(p.zenith_tint.iter().zip(flat.iter())) {
            let want = zt * 0.5 + ft * 0.5;
            assert!(close(*zv, want), "半阴天顶 = 0.5*(z+flat): {zv} vs {want}");
        }
        let sun_half = sky_radiance(p.sun_dir, &half);
        assert!(sun_half[0] > z[0], "overcast=0.5 盘仍占优");
    }

    #[test]
    fn zero_and_non_finite_directions_are_honest_zero() {
        let p = clear();
        assert_eq!(sky_radiance([0.0; 3], &p), [0.0; 3]);
        assert_eq!(sky_radiance([f32::NAN, 0.0, 1.0], &p), [0.0; 3]);
        assert_eq!(sky_radiance([f32::INFINITY, 0.0, 0.0], &p), [0.0; 3]);
        assert_eq!(sky_radiance([1.0, 0.0, f32::NEG_INFINITY], &p), [0.0; 3]);
    }

    #[test]
    fn hdr_radiance_exceeds_one() {
        let p = clear();
        for s in sky_radiance(p.sun_dir, &p).iter() {
            assert!(*s > 1.0, "线性 HDR 太阳盘应 >1: {s}");
        }
        // 梯度本身也可以 >1:tint 直接过 1,天顶逐位带出。
        let bright = SkyParams {
            zenith_tint: [1.4, 1.1, 0.9],
            ..p
        };
        assert_eq!(sky_radiance([0.0, 1.0, 0.0], &bright), [1.4, 1.1, 0.9]);
    }

    #[test]
    fn env_table_row_order_is_documented_and_deterministic() {
        let p = clear();
        let t = env_from_sky(&p, 3);
        assert_eq!(t.len(), 3 * ENV_AZIMUTHS as usize);
        // 环序:y 从天顶向天底递减;环内 8 行共享同一 y(逐位)。
        for (ring, rows) in t.chunks(ENV_AZIMUTHS as usize).enumerate() {
            let y0 = rows[0].0[1];
            let want_y = (std::f32::consts::PI * (ring as f32 + 0.5) / 3.0).cos();
            for (j, (dir, rad)) in rows.iter().enumerate() {
                assert_eq!(dir[1], y0, "环内 y 逐位一致");
                assert!(close(y0, want_y), "环 {ring} 的 y 应是 cos(θ_环中点)");
                assert_eq!(
                    *rad,
                    sky_radiance(*dir, &p),
                    "行辐射 = sky_radiance(行方向)"
                );
                let n = dot3(*dir, *dir).sqrt();
                assert!(close(n, 1.0), "方向单位长度: {n}");
                if j == 0 {
                    assert_eq!(dir[0], 0.0, "φ=0 指向 +z:x 分量为 0");
                    assert!(dir[2] > 0.0);
                } else if j == 1 {
                    assert!(dir[0] > 0.0, "φ 增大先偏向 +x");
                }
            }
        }
        let az = ENV_AZIMUTHS as usize;
        assert!(t[0].0[1] > t[az].0[1], "y 沿环序递减");
        assert!(t[az].0[1] > t[2 * az].0[1]);
        // bands=0 → 空表;超过上限夹到 256。
        assert!(env_from_sky(&p, 0).is_empty());
        assert_eq!(
            env_from_sky(&p, u32::MAX).len(),
            ENV_MAX_BANDS as usize * ENV_AZIMUTHS as usize
        );
    }

    #[test]
    fn double_run_is_bit_identical() {
        let p = clear();
        let dirs = [
            [0.0, 1.0, 0.0],
            [0.6, 0.8, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, -1.0, 0.0],
            [0.3, -0.4, 0.5],
            [-0.2, 0.9, 0.1],
        ];
        let to_radiance = |pp: &SkyParams| -> Vec<[f32; 3]> {
            dirs.iter().map(|d| sky_radiance(*d, pp)).collect()
        };
        assert_eq!(to_radiance(&p), to_radiance(&p), "sky_radiance 双跑逐位");
        assert_eq!(env_from_sky(&p, 7), env_from_sky(&p, 7), "env 表双跑逐位");
        let oc = SkyParams { overcast: 0.7, ..p };
        assert_eq!(to_radiance(&oc), to_radiance(&oc), "阴天变体同样逐位");
    }

    #[test]
    fn default_is_a_valid_clear_sky() {
        let p = SkyParams::default();
        assert!(close(dot3(p.sun_dir, p.sun_dir).sqrt(), 1.0));
        let sun = sky_radiance(p.sun_dir, &p);
        assert!(sun.iter().all(|s| *s > 1.0), "默认太阳是 HDR: {sun:?}");
        assert!(
            sky_radiance([0.0, 1.0, 0.0], &p)[2] < sun[2],
            "太阳亮过天顶"
        );
    }
}
