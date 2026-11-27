//! L5 环境光辐照与色调映射(feature `ibl`,默认关)——真实场景批次 WP2。
//!
//! 纯 CPU 数学,补"环境亮度怎么进表面、HDR 怎么进屏幕"这两步:
//! [`lambert_irradiance`] 把调用方注入的环境辐射按 cos 权重积分成一个
//! 辐照度(单位与辐射相同,纯相对量);[`tonemap_aces`] 把线性 HDR 压进
//! 显示空间。两者都是逐向量运算,无矩阵(所以不涉及 `crate::camera::Mat4`
//! 的约定)、无 GPU、无同步原语,headless 可测。
//!
//! **契约**
//! - 纯 CPU、headless 可测,零新增依赖,只用 `std`,wasm32 可编译;
//! - 坐标 y 向上;方向一律是"从被照点指向环境"的单位向量(与法线同侧
//!   为正),不是光的行进方向;
//! - 环境的来源完全由调用方注入([`RadianceFn`]):立方体贴图查找、程序化
//!   天空、解析光源都行;本模块不读贴图、不 `use` 同批次其它模块
//!   (`shadow` / `cull` / `instancing` / `sky` / `scene`);
//! - 确定性:无随机、无哈希迭代序、固定累加顺序,同一输入两次跑逐位
//!   一致。跨平台只承诺*结构*一致(方向集的分层与数量),见诚实边界。
//!
//! **诚实边界**
//! - [`lambert_irradiance`] 是对连续环境的**离散采样积分**:误差随采样
//!   密度下降,但对点光源式的尖锐亮斑会欠采样漏光;它是 Lambert(漫反射)
//!   的估计,没有镜面/glossy 项,不代入 BRDF;
//! - [`default_sample_dirs`] 是固定的低分辨率分层(34 向),够测试与初版
//!   渲染;更高保真请由调用方自备方向集,接口不锁死;
//! - [`lambert_irradiance`] 假定 `sample_dirs` 是单位向量但**不做内部
//!   归一化**(省一次 sqrt):非单位方向会被静默错加权,调用方自检;
//! - [`tonemap_aces`] 是 Narkowicz 2015 的**有理拟合近似**,不是 ACES 的
//!   RRT+ODT 全流程:色相/饱和度保真不及真 ACES;渐近线 2.51/2.43 ≈
//!   1.033,过 gamma 后 ≈ 1.0148 —— 输入超过 ≈ 7.24 后输出**略超 1.0**,
//!   需要严格 [0,1] 的调用方自行 clamp;
//! - 方位角的 sin/cos 用目标平台的 libm:同平台双跑逐位一致,跨平台
//!   可能差 1 ulp(集合结构不受影响)。

/// 环境辐射查询:方向(单位向量)→ 该方向上的线性 HDR 辐射。
///
/// 由调用方注入。单位是调用方的单位,辐照度结果与之同单位(纯相对量,
/// 引擎不解释 W/sr/m²)。要保确定性,该函数应是纯的:同方向两次查询
/// 逐位一致,不依赖随机或全局可变状态。
pub type RadianceFn<'a> = dyn Fn([f32; 3]) -> [f32; 3] + 'a;

/// 确定性方向集:6 轴向 + 3 条纬度环,共 **34** 个单位向量(y 向上)。
///
/// 生成公式(完全固定,无随机;生成顺序即下述顺序):
/// - 轴向 6 个:+X、−X、+Y、−Y、+Z、−Z(归一化除以 1.0,仍是精确单轴);
/// - 赤道环(仰角 0°):12 等分方位角,θ_k = 15° + k·30°,k = 0..12
///   —— 半个步长的偏移,避免与 ±X/±Z 轴向重合;
/// - 上环(仰角 +45°)、下环(仰角 −45°):各 8 等分方位角,θ_k = k·45°;
/// - 环上方向 = normalize([cos(仰角)·cos θ, sin(仰角), cos(仰角)·sin θ])。
///
/// 上、下环逐方位角镜像 + 赤道环均分,整个集合各向无偏(Σd ≈ 0)。
pub fn default_sample_dirs() -> Vec<[f32; 3]> {
    let mut dirs = Vec::with_capacity(34);
    for axis in 0..3usize {
        for sign in [1.0_f32, -1.0] {
            let mut d = [0.0_f32; 3];
            d[axis] = sign;
            dirs.push(normalize(d));
        }
    }
    // 赤道环:仰角 0°(sin=0, cos=1),12 等分,起点偏移半个步长(15°)。
    push_ring(&mut dirs, 0.0, 1.0, 12, std::f32::consts::FRAC_PI_6 / 2.0);
    // 上、下环:仰角 ±45°(sin=cos=√2/2),8 等分,从 0° 起。
    let r45 = std::f32::consts::FRAC_1_SQRT_2;
    push_ring(&mut dirs, r45, r45, 8, 0.0);
    push_ring(&mut dirs, -r45, r45, 8, 0.0);
    dirs
}

/// 一条纬度环:传入 sin/cos(仰角)、经度等分数与方位角起点(弧度)。
fn push_ring(dirs: &mut Vec<[f32; 3]>, sin_elev: f32, cos_elev: f32, count: usize, start: f32) {
    let step = std::f32::consts::TAU / count as f32;
    for k in 0..count {
        let az = start + k as f32 * step;
        dirs.push(normalize([
            cos_elev * az.cos(),
            sin_elev,
            cos_elev * az.sin(),
        ]));
    }
}

/// 向量归一化;零向量原样返回(调用方不会传,防御而已)。
fn normalize(v: [f32; 3]) -> [f32; 3] {
    let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if n > 0.0 {
        [v[0] / n, v[1] / n, v[2] / n]
    } else {
        v
    }
}

/// Lambert(漫反射)环境辐照度:对注入环境做 cos 加权平均的离散估计。
///
/// `sample_dirs` 是单位方向集(可用 [`default_sample_dirs`],也可调用方
/// 自备);`radiance` 注入环境;`normal` 是表面法线,**不必归一化**——
/// 权重 `w = max(0, normal·d)` 出现在分子分母两侧,正数缩放互相抵消。
///
/// 结果 = Σ(w·L(d)) / Σw。**诚实语义**:当 Σw == 0——法线背对所有采样
/// 方向(全遮挡,或方向集只覆盖对面半球)——返回 `[0,0,0]`,不编造环境
/// 光。权重为零的方向不查询 `radiance`:物理上不可见,省一次调用;顺带
/// 避开 IEEE 的 0·Inf = NaN 陷阱,看不见的方向不会毒化结果。
///
/// 非有限输入的显式定义(不 panic):
/// - `normal` 含 NaN:点积为 NaN,`max(0, NaN)` 按定义取 0,权重归零;
///   若因此 Σw == 0,输出 `[0,0,0]`;
/// - `radiance` 返回 NaN / ±Inf(可见方向):按 IEEE 传播,输出就是
///   NaN / ±Inf——病灶显性化,不静默涂黑。
pub fn lambert_irradiance(
    sample_dirs: &[[f32; 3]],
    radiance: &RadianceFn<'_>,
    normal: [f32; 3],
) -> [f32; 3] {
    let mut num = [0.0_f32; 3];
    let mut den = 0.0_f32;
    for &d in sample_dirs {
        let w = (normal[0] * d[0] + normal[1] * d[1] + normal[2] * d[2]).max(0.0);
        if w == 0.0 {
            continue;
        }
        let l = radiance(d);
        den += w;
        num[0] += w * l[0];
        num[1] += w * l[1];
        num[2] += w * l[2];
    }
    if den == 0.0 {
        return [0.0; 3];
    }
    [num[0] / den, num[1] / den, num[2] / den]
}

/// ACES 色调映射(Narkowicz 2015 拟合,逐通道)+ gamma 2.2。
///
/// 输入线性 HDR(名义域 [0, +∞)),输出显示空间(近似 [0, 1.0148])。
/// 每通道独立套同一条曲线:
///
/// ```text
/// f(x) = (x·(2.51·x + 0.03)) / (x·(2.43·x + 0.59) + 0.14)   -- ACES 近似
/// out  = f(x)^(1 / 2.2)                                      -- gamma 2.2
/// ```
///
/// 系数 2.51 / 0.03 / 2.43 / 0.59 / 0.14 来自 Narkowicz 2015(ACES
/// Filmic Tone Mapping Curve,对 ACES RRT+ODT 的最小二乘有理拟合)。
/// 手算锚点:0 → 0;1.0 → 2.54/3.16 = 0.803797…,^(1/2.2) = 0.905492…。
///
/// 非有限/域外输入的显式定义(不 panic):NaN 入 → NaN 出;+Inf 入经
/// Inf/Inf → NaN;负输入无物理意义但仍有定义(如 −1 → 1.108,接近 0
/// 的负数因负底开 gamma 次方得 NaN)。
pub fn tonemap_aces(c: [f32; 3]) -> [f32; 3] {
    let curve = |x: f32| -> f32 {
        let mapped = (x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14);
        mapped.powf(1.0 / 2.2)
    };
    [curve(c[0]), curve(c[1]), curve(c[2])]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 三通道逐个落进容差带。
    fn assert_close(a: [f32; 3], b: [f32; 3], tol: f32) {
        for i in 0..3 {
            assert!(
                (a[i] - b[i]).abs() < tol,
                "通道 {i}: {a:?} 偏离 {b:?} 超过 {tol}"
            );
        }
    }

    #[test]
    fn uniform_environment_yields_the_constant_on_any_normal() {
        let dirs = default_sample_dirs();
        let env = |_d: [f32; 3]| [0.7, 0.35, 1.4];
        // 任意法线(朝上、朝下、水平、斜向)都该还原常数环境。
        let normals = [
            [0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.267, -0.535, 0.802],
        ];
        for n in normals {
            assert_close(lambert_irradiance(&dirs, &env, n), [0.7, 0.35, 1.4], 1e-3);
        }
        // 常数环境与采样集无关:自备的 4 向稀疏集同样还原。
        let sparse = [
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.6, 0.8, 0.0],
        ];
        assert_close(
            lambert_irradiance(&sparse, &env, [0.0, 1.0, 0.0]),
            [0.7, 0.35, 1.4],
            1e-3,
        );
    }

    #[test]
    fn zenith_gradient_lights_up_normals_more_than_down() {
        let dirs = default_sample_dirs();
        // 线性天顶梯度:越朝上越亮,且处处非负。
        let env = |d: [f32; 3]| {
            let t = 0.5 * (d[1] + 1.0); // d.y ∈ [-1,1] → t ∈ [0,1]
            [0.1 + 4.0 * t; 3]
        };
        let up = lambert_irradiance(&dirs, &env, [0.0, 1.0, 0.0]);
        let side = lambert_irradiance(&dirs, &env, [1.0, 0.0, 0.0]);
        let down = lambert_irradiance(&dirs, &env, [0.0, -1.0, 0.0]);
        assert!(up[0] > side[0], "朝上应亮过水平: {} vs {}", up[0], side[0]);
        assert!(
            side[0] > down[0],
            "水平应亮过朝下: {} vs {}",
            side[0],
            down[0]
        );
    }

    #[test]
    fn sky_bright_ground_dark_keeps_hemisphere_sign() {
        let dirs = default_sample_dirs();
        let env = |d: [f32; 3]| {
            if d[1] >= 0.0 {
                [5.0; 3]
            } else {
                [0.05; 3]
            }
        };
        let up = lambert_irradiance(&dirs, &env, [0.0, 1.0, 0.0]);
        let down = lambert_irradiance(&dirs, &env, [0.0, -1.0, 0.0]);
        assert!(up[0] > 4.9, "天顶亮度应基本保全: {}", up[0]);
        assert!(down[0] < 0.1, "朝下只收到地面漏光: {}", down[0]);
        assert!(up[0] > 10.0 * down[0], "半球差异符号必须正确");
    }

    #[test]
    fn normal_length_cancels_in_the_weighted_average() {
        let dirs = default_sample_dirs();
        let env = |d: [f32; 3]| [1.0 + d[1], 0.5, 1.0 - d[1]];
        let a = lambert_irradiance(&dirs, &env, [0.0, 1.0, 0.0]);
        let b = lambert_irradiance(&dirs, &env, [0.0, 5.0, 0.0]);
        // 权重同乘 5,分子分母同消(容差给 f32 舍入)。
        assert_close(a, b, 1e-3);
    }

    #[test]
    fn fully_occluded_normal_returns_zero_not_garbage() {
        // 方向集只覆盖上半球:朝下的法线背对一切,Σw == 0。
        let upper = [
            [0.0, 1.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.6, 0.8, 0.0],
        ];
        let env = |_d: [f32; 3]| [10.0; 3];
        assert_eq!(
            lambert_irradiance(&upper, &env, [0.0, -1.0, 0.0]),
            [0.0, 0.0, 0.0],
            "全遮挡必须诚实返回全零"
        );
        // 空方向集同理。
        let empty: [[f32; 3]; 0] = [];
        assert_eq!(lambert_irradiance(&empty, &env, [0.0, 1.0, 0.0]), [0.0; 3]);
        // NaN 法线:每条权重 max(0, NaN) = 0 → Σw == 0 → 全零(显式定义)。
        assert_eq!(lambert_irradiance(&upper, &env, [f32::NAN; 3]), [0.0; 3]);
    }

    #[test]
    fn non_finite_radiance_propagates_without_panicking() {
        let dirs = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let nan_env = |_d: [f32; 3]| [f32::NAN, 1.0, 2.0];
        let e = lambert_irradiance(&dirs, &nan_env, [1.0, 0.0, 0.0]);
        assert!(e[0].is_nan(), "NaN 辐射按 IEEE 传播,不静默清零");
        assert_eq!((e[1], e[2]), (1.0, 2.0), "污染只限 NaN 所在通道");
        let inf_env = |_d: [f32; 3]| [f32::INFINITY, 1.0, 1.0];
        let e = lambert_irradiance(&dirs, &inf_env, [1.0, 0.0, 0.0]);
        assert!(e[0].is_infinite(), "+Inf 显性传播");
        assert_eq!(e[1], 1.0);
    }

    #[test]
    fn tonemap_zero_maps_to_zero() {
        assert_eq!(tonemap_aces([0.0, 0.0, 0.0]), [0.0, 0.0, 0.0]);
    }

    #[test]
    fn tonemap_one_matches_the_hand_computed_constant() {
        // 手算:x=1 → 2.54/3.16 = 0.8037974…,^(1/2.2) = 0.9054924…。
        let out = tonemap_aces([1.0, 1.0, 1.0]);
        for v in out {
            assert!((v - 0.905_492_4).abs() < 1e-4, "got {v}");
        }
    }

    #[test]
    fn tonemap_curve_is_monotonic_on_the_working_range() {
        // f32 在渐近线(≈1.0148)附近只剩 1 ulp 的平坦抖动,单调断言取
        // 拟合正常工作的 [0, 50] 区间;尾部平坦区单独测压缩。
        let mut prev = [0.0_f32; 3];
        let steps = 250usize;
        for k in 1..=steps {
            let x = 50.0 * k as f32 / steps as f32;
            let out = tonemap_aces([x, x, x]);
            for (ch, &v) in out.iter().enumerate() {
                assert!(v >= prev[ch], "x={x} 通道 {ch}: {v} < {}", prev[ch]);
            }
            prev = out;
        }
    }

    #[test]
    fn tonemap_compresses_large_values_toward_the_asymptote() {
        let t4 = tonemap_aces([4.0; 3])[0];
        let t10 = tonemap_aces([10.0; 3])[0];
        let t1000 = tonemap_aces([1000.0; 3])[0];
        // 手算锚点:4 → 0.98783;渐近线 (2.51/2.43)^(1/2.2) ≈ 1.01483。
        assert!((t4 - 0.987_83).abs() < 1e-4, "got {t4}");
        assert!(t10 > t4 && t1000 > t10, "尾部仍单调");
        // 输入放大 100 倍,输出只挪 ≈0.0106 —— 高亮被压扁。
        assert!(t1000 - t10 < 0.02, "压缩失效: {}", t1000 - t10);
        // 渐近线略超 1(诚实边界),但不会飞出去。
        assert!(t1000 > 1.0 && t1000 < 1.02, "got {t1000}");
    }

    #[test]
    fn tonemap_non_finite_inputs_are_defined_and_dont_panic() {
        let out = tonemap_aces([f32::NAN, f32::INFINITY, -1.0]);
        assert!(out[0].is_nan(), "NaN 入 → NaN 出");
        assert!(out[1].is_nan(), "+Inf 经 Inf/Inf → NaN(显式定义)");
        assert!(out[2].is_finite() && out[2] > 1.0, "负输入域外但仍有定义");
        // 接近 0 的负数:负底开 gamma 次方 → NaN(显式定义,不 panic)。
        assert!(tonemap_aces([-0.01, -0.01, -0.01])[0].is_nan());
    }

    #[test]
    fn default_dirs_follow_the_documented_layout() {
        let dirs = default_sample_dirs();
        assert_eq!(dirs.len(), 34, "6 轴向 + 12 赤道 + 8 上环 + 8 下环");
        // 全部单位长度。
        for d in &dirs {
            let n = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            assert!((n - 1.0).abs() < 1e-5, "|d| = {n}");
        }
        // 生成顺序即文档顺序:轴向在前,且归一化后仍是精确单轴。
        assert_eq!(
            &dirs[0..6],
            &[
                [1.0, 0.0, 0.0],
                [-1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, -1.0, 0.0],
                [0.0, 0.0, 1.0],
                [0.0, 0.0, -1.0],
            ]
        );
        // 赤道环 12 条 |d.y| = 0,首条方位角 15°(半个步长偏移)。
        for d in &dirs[6..18] {
            assert!(d[1].abs() < 1e-6, "赤道环混进了非赤道方向 {d:?}");
        }
        let e0 = dirs[6];
        assert!(
            (e0[0] - 15f32.to_radians().cos()).abs() < 1e-5,
            "got {e0:?}"
        );
        assert!(
            (e0[2] - 15f32.to_radians().sin()).abs() < 1e-5,
            "got {e0:?}"
        );
        // 上、下环各 8 条,仰角 ±45°;上环首条在 +X 方向仰起。
        let r45 = std::f32::consts::FRAC_1_SQRT_2;
        for d in &dirs[18..26] {
            assert!((d[1] - r45).abs() < 1e-5);
        }
        for d in &dirs[26..34] {
            assert!((d[1] + r45).abs() < 1e-5);
        }
        let u0 = dirs[18];
        assert!(
            (u0[0] - r45).abs() < 1e-5 && u0[2].abs() < 1e-5,
            "got {u0:?}"
        );
    }

    #[test]
    fn default_dirs_have_no_directional_bias() {
        let dirs = default_sample_dirs();
        // 上/下环镜像 + 赤道均分 + 轴向成对 → Σd ≈ 0。
        let mut sum = [0.0_f32; 3];
        for d in &dirs {
            for i in 0..3 {
                sum[i] += d[i];
            }
        }
        assert!(sum[0].abs() < 1e-4 && sum[1].abs() < 1e-4 && sum[2].abs() < 1e-4);
        // 方向两两不同(最近的也只到赤道首向与 +X 轴的 cos15° ≈ 0.966)。
        for (i, a) in dirs.iter().enumerate() {
            for b in &dirs[i + 1..] {
                let dot = a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
                assert!(dot < 0.999, "重复方向 {a:?} {b:?}");
            }
        }
    }

    #[test]
    fn double_run_is_bit_identical() {
        let a = default_sample_dirs();
        let b = default_sample_dirs();
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(
                x.map(f32::to_bits),
                y.map(f32::to_bits),
                "方向集两次生成必须逐位一致"
            );
        }
        let env = |d: [f32; 3]| [1.0 + d[1], 0.5, d[2] * d[2]];
        let n = [0.3, 0.9, 0.2];
        let e1 = lambert_irradiance(&a, &env, n);
        let e2 = lambert_irradiance(&b, &env, n);
        assert_eq!(e1.map(f32::to_bits), e2.map(f32::to_bits));
        let t1 = tonemap_aces(e1);
        let t2 = tonemap_aces(e2);
        assert_eq!(t1.map(f32::to_bits), t2.map(f32::to_bits));
    }
}
