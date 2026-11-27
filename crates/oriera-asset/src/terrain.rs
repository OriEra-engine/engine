//! L4 程序化地形:种子化高度场与网格构建——真实场景批次 WP5。
//!
//! [`Terrain::generate`] 用整数格点值噪声 + fBm(分形布朗运动)从 `seed`
//! 确定性生成高度场;[`Terrain::sample_bilinear`] / [`Terrain::normal_at`]
//! 查询地表;[`Terrain::build_mesh`] 输出三角网格。纯 std、wasm32 可编译、
//! 零依赖;同种子同 spec 两次生成 `heights` 逐位一致。
//!
//! **契约**
//! - 确定性:格点哈希是自写 FNV-1a 64 变体(把格点坐标、八度、seed 逐字节
//!   喂入,不用 `DefaultHasher`);fBm 按八度升序累加、插值次序固定,除
//!   浮点求和的固定顺序外无隐藏状态——IEEE-754 语义一致的目标
//!   (native / wasm32)上结果逐位相同。
//! - 坐标 y 向上,世界原点在 (0,0):格点 (ix, iz) → 世界
//!   `(ix·cell, h, iz·cell)`。位置一律 `[f32; 3]`,与 L5 `camera` /
//!   `scene` 词汇对齐。
//! - 不 `use` 同批次其它模块(`refs` / `scatter`):散布侧只经公开的
//!   [`Terrain::sample_bilinear`] / [`Terrain::normal_at`] 取地表,不读
//!   本模块内部字段。
//!
//! **噪声公式**(O = octaves,g = gain,l = lacunarity,W = 基准波长,
//! 常量见 [`GAIN`] / [`LACUNARITY`] / [`BASE_WAVELENGTH`]):
//!
//! ```text
//! h(ix, iz) = height_scale · ( Σ_{o=0..O-1} g^o · v(ix·l^o/W, iz·l^o/W, o, seed) )
//!                     / ( Σ_{o=0..O-1} g^o )
//! ```
//!
//! `v` 是值噪声:四格点各取自 [`lattice_value`] ∈ [0,1),格内坐标过
//! smoothstep(3t²−2t³)后双线性。振幅和归一使结果落在 [0,1],再乘
//! `height_scale` → 高度恒在 [0, height_scale](f32 舍入下也成立,见
//! `heights_stay_within_scale` 测试)。
//!
//! **诚实边界**
//! - 这是最基础的程序地形:无侵蚀、无域扭曲、无河流/洞窟/生物群系;
//!   高度场是单值表面——同一 (x,z) 只有一个高度,无悬崖、无悬挑。产物
//!   是"平滑丘陵 + 细节噪声",不是真实地貌。
//! - 采样双线性,C0 而非 C1:`normal_at` 的中央差分给出逐格常数梯度,
//!   法线跨格线跳变;渲染需要平滑法线须另做插值。
//! - `normal_at` 的差分步长 e = `cell`:场边界退化为单侧差分(出界采样
//!   = 边缘外推)。
//! - 格点坐标 > 2²⁴ 时 ix→f32 不可精确表示,噪声特征位置被量化(确定性
//!   不受影响);`heights` 是稠密 `size_x × size_z` f32,大规格内存
//!   (~16k×16k ≈ 1 GB)与 `build_mesh` 索引 `u32` 上限(顶点数 > 2³²)
//!   本模块都不拦截,规模由调用方把握。
//! - `build_mesh` 单分辨率:每 `step` 个格点取一个顶点;`size−1` 不整除
//!   `step` 时网格止于最后一条整格线,覆盖略小于场范围(要 LOD 就以不同
//!   `step` 多次调用)。不带 UV / 顶点法线属性。

/// FNV-1a 64 offset basis(哈希变体的起点)。
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64 素数。
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
/// fBm 基准波长,单位 = 格距 `cell`:八度 0 的噪声沿一个波长完成一次
/// smoothstep 起伏;波长越大地形越平缓。
const BASE_WAVELENGTH: f32 = 16.0;
/// fBm 振幅衰减(gain):每八度振幅 ×= 0.5。
const GAIN: f32 = 0.5;
/// fBm 频率倍增(lacunarity):每八度频率 ×= 2(2 的幂在 f32 中精确)。
const LACUNARITY: f32 = 2.0;

/// 把格点坐标、八度与 seed 混成一个 64 位哈希——FNV-1a 64 变体:四个
/// u64 的小端字节逐字节 `xor` 后乘素数。纯函数,与运行次序无关。
fn lattice_hash(ix: i64, iz: i64, octave: u64, seed: u64) -> u64 {
    let mut h = FNV_OFFSET_BASIS;
    for v in [ix as u64, iz as u64, octave, seed] {
        for b in v.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(FNV_PRIME);
        }
    }
    h
}

/// 格点噪声值:哈希高 24 位 → [0, 1)(除以 2²⁴;24 位恰为 f32 尾数宽度,
/// 每一步都可精确表示,永不舍入到 1.0)。
fn lattice_value(ix: i64, iz: i64, octave: u64, seed: u64) -> f32 {
    (lattice_hash(ix, iz, octave, seed) >> 40) as f32 / 16_777_216.0
}

/// 线性插值 `a + (b−a)·t`。t = 0 / 1 且操作数有限时精确返回 a / b——
/// 这是格点采样逐位精确的基础。
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// 单八度二维值噪声。`x` / `z` 为噪声空间坐标:取整格点值,格内分数
/// 坐标过 smoothstep(3t²−2t³,格点处一阶导为零)后双线性混合。
fn value_noise(x: f32, z: f32, octave: u64, seed: u64) -> f32 {
    let (xi, zi) = (x.floor(), z.floor());
    let (fx, fz) = (x - xi, z - zi);
    let sx = fx * fx * (3.0 - 2.0 * fx);
    let sz = fz * fz * (3.0 - 2.0 * fz);
    let (xi, zi) = (xi as i64, zi as i64);
    let u0 = lerp(
        lattice_value(xi, zi, octave, seed),
        lattice_value(xi + 1, zi, octave, seed),
        sx,
    );
    let u1 = lerp(
        lattice_value(xi, zi + 1, octave, seed),
        lattice_value(xi + 1, zi + 1, octave, seed),
        sx,
    );
    lerp(u0, u1, sz)
}

/// 地形规格。字段私有,经构造器钳制与校验(见 [`TerrainSpec::new`]),
/// 只读访问走 getter。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TerrainSpec {
    size_x: u32,
    size_z: u32,
    cell: f32,
    seed: u64,
    octaves: u8,
    height_scale: f32,
}

impl TerrainSpec {
    /// 构造地形规格。
    ///
    /// - `size_x` / `size_z`:格点顶点数,0 钳到 1(至少一个顶点)。
    /// - `cell`:格距(世界单位),必须有限且 > 0,否则 panic——规格错误
    ///   要 fail fast,没有可钳的合理值。
    /// - `octaves`:fBm 八度数,**构造时钳制到 1..=8**(0 与 >8 都收进
    ///   界内):上限挡住成本与浮点误差随八度无界增长。
    /// - `height_scale`:最大高度(世界单位);负数与非有限(NaN/±inf)
    ///   一律按 0 处理(平地)。
    /// - `seed`:噪声种子;同种子同规格 → 地形逐位一致。
    pub fn new(
        size_x: u32,
        size_z: u32,
        cell: f32,
        seed: u64,
        octaves: u8,
        height_scale: f32,
    ) -> Self {
        assert!(
            cell.is_finite() && cell > 0.0,
            "terrain cell must be finite and > 0, got {cell}"
        );
        Self {
            size_x: size_x.max(1),
            size_z: size_z.max(1),
            cell,
            seed,
            octaves: octaves.clamp(1, 8),
            height_scale: if height_scale.is_finite() {
                height_scale.max(0.0)
            } else {
                0.0
            },
        }
    }

    /// x 向格点数(≥ 1)。
    pub fn size_x(&self) -> u32 {
        self.size_x
    }

    /// z 向格点数(≥ 1)。
    pub fn size_z(&self) -> u32 {
        self.size_z
    }

    /// 格距(世界单位,> 0)。
    pub fn cell(&self) -> f32 {
        self.cell
    }

    /// 噪声种子。
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// fBm 八度数(构造时已钳到 1..=8)。
    pub fn octaves(&self) -> u8 {
        self.octaves
    }

    /// 最大高度(≥ 0,构造时已归一)。
    pub fn height_scale(&self) -> f32 {
        self.height_scale
    }
}

/// 种子化高度场地形。经 [`Terrain::generate`] 构造,内部只读;查询走
/// 公开采样接口。
#[derive(Debug, Clone)]
pub struct Terrain {
    spec: TerrainSpec,
    /// 行优先高度:`heights[iz·size_x + ix]`(z 行、x 列)。
    heights: Vec<f32>,
}

impl Terrain {
    /// 按规格生成地形。每个格点按模块文档公式逐八度累加 fBm,归一后乘
    /// `height_scale`;累加与归一的次序固定,结果恒在 [0, height_scale]。
    pub fn generate(spec: TerrainSpec) -> Self {
        let mut heights = Vec::with_capacity(spec.size_x as usize * spec.size_z as usize);
        for iz in 0..spec.size_z {
            for ix in 0..spec.size_x {
                let (mut amp, mut sum, mut norm) = (1.0f32, 0.0f32, 0.0f32);
                for octave in 0..spec.octaves {
                    // lacunarity 2:每八度频率加倍(2 的幂,f32 精确)
                    let freq = LACUNARITY.powi(i32::from(octave));
                    let nx = ix as f32 * freq / BASE_WAVELENGTH;
                    let nz = iz as f32 * freq / BASE_WAVELENGTH;
                    sum += amp * value_noise(nx, nz, u64::from(octave), spec.seed);
                    norm += amp;
                    amp *= GAIN;
                }
                heights.push(sum / norm * spec.height_scale);
            }
        }
        Self { spec, heights }
    }

    /// 世界范围 [x, z] = ((size_x−1)·cell, (size_z−1)·cell)。散布的
    /// 抖动网格按它分格。
    pub fn extent(&self) -> [f32; 2] {
        [
            (self.spec.size_x - 1) as f32 * self.spec.cell,
            (self.spec.size_z - 1) as f32 * self.spec.cell,
        ]
    }

    /// 行优先高度切片:`heights()[iz·size_x + ix]`(z 行、x 列)。
    pub fn heights(&self) -> &[f32] {
        &self.heights
    }

    /// 地形规格(只读)。
    pub fn spec(&self) -> &TerrainSpec {
        &self.spec
    }

    /// 高度场双线性采样。`x` / `z` 为世界坐标;出界 = 边缘外推(坐标钳
    /// 入场范围,取边缘值)。格点顶点处(x = ix·cell 且除法能精确还原格
    /// 坐标,如整数/2 的幂格距)逐位返回存储高度。
    pub fn sample_bilinear(&self, x: f32, z: f32) -> f32 {
        let gx = (x / self.spec.cell).clamp(0.0, (self.spec.size_x - 1) as f32);
        let gz = (z / self.spec.cell).clamp(0.0, (self.spec.size_z - 1) as f32);
        let x0 = gx.floor() as usize;
        let z0 = gz.floor() as usize;
        let x1 = (x0 + 1).min(self.spec.size_x as usize - 1);
        let z1 = (z0 + 1).min(self.spec.size_z as usize - 1);
        let tx = gx - x0 as f32;
        let tz = gz - z0 as f32;
        let sx = self.spec.size_x as usize;
        let u0 = lerp(self.heights[z0 * sx + x0], self.heights[z0 * sx + x1], tx);
        let u1 = lerp(self.heights[z1 * sx + x0], self.heights[z1 * sx + x1], tx);
        lerp(u0, u1, tz)
    }

    /// 地表法线(y 向上,单位长度)。中央差分,步长 e = `cell`(一格距,
    /// 与分段线性采样的分辨率一致):∂h/∂x ≈ (h(x+e) − h(x−e)) / 2e,
    /// 法线 = [−∂h/∂x, 1, −∂h/∂z] 归一。场边界因出界外推退化为单侧差分。
    pub fn normal_at(&self, x: f32, z: f32) -> [f32; 3] {
        let e = self.spec.cell;
        let dhdx = (self.sample_bilinear(x + e, z) - self.sample_bilinear(x - e, z)) / (2.0 * e);
        let dhdz = (self.sample_bilinear(x, z + e) - self.sample_bilinear(x, z - e)) / (2.0 * e);
        let n = [-dhdx, 1.0, -dhdz];
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        [n[0] / len, n[1] / len, n[2] / len]
    }

    /// 三角化网格。每 `step` 个格点取一个顶点(`step` < 1 按 1 计,0 亦
    /// 然);每轴顶点数 = (size−1)/step + 1;索引数 = 6 × (nx−1) × (nz−1)。
    /// `size−1` 不整除 `step` 时网格止于最后一条整格线(见模块文档)。
    /// nx 或 nz < 2 的退化网格没有四边形,索引为空。
    pub fn build_mesh(&self, step: u32) -> TerrainMesh {
        let step = step.max(1) as usize;
        let sx = self.spec.size_x as usize;
        let sz = self.spec.size_z as usize;
        let nx = (sx - 1) / step + 1;
        let nz = (sz - 1) / step + 1;
        let mut mesh = TerrainMesh {
            positions: Vec::with_capacity(nx * nz),
            indices: Vec::with_capacity(6 * (nx - 1) * (nz - 1)),
        };
        for iz in (0..sz).step_by(step) {
            for ix in (0..sx).step_by(step) {
                mesh.positions.push([
                    ix as f32 * self.spec.cell,
                    self.heights[iz * sx + ix],
                    iz as f32 * self.spec.cell,
                ]);
            }
        }
        if nx >= 2 && nz >= 2 {
            for r in 0..nz - 1 {
                for c in 0..nx - 1 {
                    // 四边形角点:a=(r,c) b=(r,c+1) c=(r+1,c) d=(r+1,c+1)。
                    // 三角 (a,c,b) / (b,c,d):从 +y 往下看逆时针(RH 坐标),
                    // 几何法线指向 +y。
                    let a = (r * nx + c) as u32;
                    let b = a + 1;
                    let c2 = a + nx as u32;
                    let d = c2 + 1;
                    mesh.indices.extend_from_slice(&[a, c2, b, b, c2, d]);
                }
            }
        }
        mesh
    }
}

/// 地形三角网格:顶点 + 三角索引。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TerrainMesh {
    /// 顶点(y 向上),行优先:z 行在外、x 列在内,顶点 (r, c) 下标
    /// = r·nx + c(r、c 为按 `step` 抽稀后的行/列号)。
    pub positions: Vec<[f32; 3]>,
    /// 三角索引,每四边形两个:`[a, c, b, b, c, d]`,从 +y 看逆时针。
    pub indices: Vec<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 常用规格:17×13 格、格距 1、4 八度、最大高度 20。
    fn spec(seed: u64) -> TerrainSpec {
        TerrainSpec::new(17, 13, 1.0, seed, 4, 20.0)
    }

    #[test]
    fn same_seed_generation_is_bit_identical() {
        let a = Terrain::generate(spec(42));
        let b = Terrain::generate(spec(42));
        assert_eq!(a.heights().len(), b.heights().len());
        for (x, y) in a.heights().iter().zip(b.heights()) {
            assert_eq!(x.to_bits(), y.to_bits(), "同种子必须逐位复现");
        }
    }

    #[test]
    fn different_seed_heights_differ() {
        let a = Terrain::generate(spec(1));
        let b = Terrain::generate(spec(2));
        assert_ne!(a.heights(), b.heights(), "不同种子必须给出不同高度场");
    }

    #[test]
    fn grid_point_sampling_is_exact() {
        let t = Terrain::generate(spec(7));
        let cell = t.spec().cell();
        let sx = t.spec().size_x() as usize;
        // 格点顶点位置逐位返回存储高度
        for iz in 0..t.spec().size_z() as usize {
            for ix in 0..sx {
                let s = t.sample_bilinear(ix as f32 * cell, iz as f32 * cell);
                assert_eq!(
                    s.to_bits(),
                    t.heights()[iz * sx + ix].to_bits(),
                    "格点 ({ix},{iz}) 采样必须精确"
                );
            }
        }
        // 出界 = 边缘外推:四角外远点各取对应边缘值
        let ext = t.extent();
        let last = t.heights().len() - 1;
        assert_eq!(
            t.sample_bilinear(-9.0, -9.0).to_bits(),
            t.heights()[0].to_bits()
        );
        assert_eq!(
            t.sample_bilinear(ext[0] + 9.0, ext[1] + 9.0).to_bits(),
            t.heights()[last].to_bits()
        );
        assert_eq!(
            t.sample_bilinear(ext[0] + 9.0, 0.0).to_bits(),
            t.heights()[sx - 1].to_bits()
        );
        assert_eq!(
            t.sample_bilinear(0.0, ext[1] + 9.0).to_bits(),
            t.heights()[(t.spec().size_z() as usize - 1) * sx].to_bits()
        );
        // 格间点落在四邻格点高度构成的盒内
        let mid = t.sample_bilinear(2.5 * cell, 3.5 * cell);
        let h = |ix: usize, iz: usize| t.heights()[iz * sx + ix];
        let lo = h(2, 3).min(h(3, 3)).min(h(2, 4)).min(h(3, 4));
        let hi = h(2, 3).max(h(3, 3)).max(h(2, 4)).max(h(3, 4));
        assert!((lo..=hi).contains(&mid));
    }

    #[test]
    fn heights_stay_within_scale() {
        let t = Terrain::generate(spec(123));
        let scale = t.spec().height_scale();
        assert!(scale > 0.0);
        for h in t.heights() {
            assert!((0.0..=scale).contains(h), "高度 {h} 超出 [0, {scale}]");
        }
    }

    #[test]
    fn constructor_clamps_octaves_size_and_scale() {
        // 0 尺寸钳到 1、八度钳到 1..=8、负/非有限高度归 0(平地)
        let s = TerrainSpec::new(0, 0, 1.0, 5, 0, -3.0);
        assert_eq!((s.size_x(), s.size_z()), (1, 1));
        assert_eq!(s.octaves(), 1);
        assert_eq!(s.height_scale(), 0.0);
        assert_eq!(TerrainSpec::new(4, 4, 1.0, 5, 99, 8.0).octaves(), 8);
        assert_eq!(
            TerrainSpec::new(4, 4, 1.0, 5, 3, f32::NAN).height_scale(),
            0.0
        );
        assert_eq!(
            TerrainSpec::new(4, 4, 1.0, 5, 3, f32::INFINITY).height_scale(),
            0.0
        );
    }

    #[test]
    #[should_panic(expected = "cell")]
    fn cell_must_be_finite_and_positive() {
        let _ = TerrainSpec::new(4, 4, 0.0, 1, 1, 1.0);
    }

    #[test]
    fn flat_terrain_normal_is_up() {
        let t = Terrain::generate(TerrainSpec::new(9, 9, 1.0, 3, 2, 0.0));
        assert_eq!(t.normal_at(4.0, 4.0), [0.0, 1.0, 0.0]);
        assert_eq!(t.normal_at(0.0, 0.0), [0.0, 1.0, 0.0]);
    }

    #[test]
    fn ramp_terrain_normal_matches_analytic() {
        // 手工组装沿 x 的斜坡(k = 2,格距 1):线性面上中央差分是精确梯度
        let spec = TerrainSpec::new(9, 9, 1.0, 1, 1, 16.0);
        let heights: Vec<f32> = (0..81).map(|i| 2.0 * (i % 9) as f32).collect();
        let t = Terrain { spec, heights };
        let n = t.normal_at(2.0, 6.0);
        let inv = 1.0 / 5.0f32.sqrt();
        let expect = [-2.0 * inv, inv, 0.0];
        for i in 0..3 {
            assert!(
                (n[i] - expect[i]).abs() < 1e-5,
                "n[{i}] = {} 偏离解析值 {expect:?}",
                n[i]
            );
        }
    }

    #[test]
    fn build_mesh_counts_follow_step_rule() {
        let t = Terrain::generate(TerrainSpec::new(9, 9, 1.0, 11, 3, 10.0));
        // step=1:全部格点,索引 = 6·(nx−1)·(nz−1)
        let m = t.build_mesh(1);
        assert_eq!(m.positions.len(), 81);
        assert_eq!(m.indices.len(), 6 * 8 * 8);
        // step=2:(9−1)/2+1 = 5 每轴
        let m = t.build_mesh(2);
        assert_eq!(m.positions.len(), 25);
        assert_eq!(m.indices.len(), 6 * 4 * 4);
        // step=7:(9−1)/7+1 = 2 每轴,恰好一个四边形
        let m = t.build_mesh(7);
        assert_eq!(m.positions.len(), 4);
        assert_eq!(m.indices.len(), 6);
        // step 大于场:1 顶点、0 索引(退化)
        let m = t.build_mesh(100);
        assert_eq!(m.positions.len(), 1);
        assert!(m.indices.is_empty());
        // step=0 按 1 计
        assert_eq!(t.build_mesh(0).positions.len(), 81);
    }

    #[test]
    fn build_mesh_row_major_and_upward_winding() {
        let t = Terrain::generate(TerrainSpec::new(9, 9, 2.0, 11, 3, 10.0));
        let m = t.build_mesh(1);
        // 行优先:顶点 1 = (x = cell, z = 0),高度 = heights[1] 逐位一致
        assert_eq!(m.positions[1][0], 2.0);
        assert_eq!(m.positions[1][2], 0.0);
        assert_eq!(m.positions[1][1].to_bits(), t.heights()[1].to_bits());
        // 首三角几何法线指向 +y(RH 坐标,与高度值无关)
        let (a, b, c) = (
            m.positions[m.indices[0] as usize],
            m.positions[m.indices[1] as usize],
            m.positions[m.indices[2] as usize],
        );
        let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let e2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let n = [
            e1[1] * e2[2] - e1[2] * e2[1],
            e1[2] * e2[0] - e1[0] * e2[2],
            e1[0] * e2[1] - e1[1] * e2[0],
        ];
        assert!(n[1] > 0.0, "绕序必须朝 +y,得到 {n:?}");
    }
}
