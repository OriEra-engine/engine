//! L5 相机与投影矩阵（#152 首刀）。
//!
//! 纯数学模块,不受 `render-wgpu` 门控;`gpu::HeadlessRenderer::render_mesh_view`
//! 是它的第一个消费者（splat 路径的 M1 也落在这块底座上,见 PLAN-splat v2）。
//!
//! 约定（与 wgpu/WebGPU 直接对齐,文档化常量式的选择）:
//! - 列向量、行主序存储:`clip = M * [p, 1]`,`clip_i = Σ_j m[i][j] * v_j`;
//! - 视空间 **y 向上、镜头看向 -z**(GL 惯例,与投影矩阵的 `w = -z_view` 配对);
//! - NDC: x/y ∈ [-1, 1],**z ∈ [0, 1]**(WebGPU 裁剪域;近平面 0、远平面 1);
//! - 像素坐标原点在左上、y 向下([`Camera::ndc_to_pixel`])。
//!
//! 注意:splat 路径的 `oriera_splat::SplatCamera` 是另一套约定(x 右 / y 下 /
//! z 前,OpenCV),那是泼溅数学的历史选择;两者不混用,统一与否待 M1 时评审。

/// 行主序 4×4 矩阵。`m[row][col]`。
pub type Mat4 = [[f32; 4]; 4];

/// 矩阵乘法 `a * b`(先 b 后 a 的作用顺序)。
pub fn mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut out = [[0.0; 4]; 4];
    for (r, row) in out.iter_mut().enumerate() {
        for (c, cell) in row.iter_mut().enumerate() {
            *cell = (0..4).map(|k| a[r][k] * b[k][c]).sum();
        }
    }
    out
}

/// 变换点 `[x, y, z, 1]`,返回裁剪坐标四元组。
pub fn transform(m: &Mat4, p: [f32; 3]) -> [f32; 4] {
    let v = [p[0], p[1], p[2], 1.0];
    (0..4)
        .map(|r| m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2] + m[r][3] * v[3])
        .collect::<Vec<_>>()
        .try_into()
        .expect("4 rows")
}

/// look_at 视图矩阵(世界 → 视空间)。镜头看向 `target`,相机 z 轴指回 `eye`
/// 方向(即视空间 -z 是「前方」)。
pub fn view_look_at(eye: [f32; 3], target: [f32; 3], up: [f32; 3]) -> Mat4 {
    let sub = |a: [f32; 3], b: [f32; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let cross = |a: [f32; 3], b: [f32; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let norm = |v: [f32; 3]| {
        let l = dot(v, v).sqrt();
        [v[0] / l, v[1] / l, v[2] / l]
    };
    let z = norm(sub(eye, target)); // 相机 z 轴 = 后方
    let mut x = cross(up, z);
    if dot(x, x) < 1e-12 {
        // up 与视线共线:换一根参考轴(gluLookAt 的经典急救)。
        let alt = if z[1].abs() < 0.9 {
            [0.0, 1.0, 0.0]
        } else {
            [1.0, 0.0, 0.0]
        };
        x = cross(alt, z);
    }
    let x = norm(x);
    let y = cross(z, x);
    [
        [x[0], x[1], x[2], -dot(x, eye)],
        [y[0], y[1], y[2], -dot(y, eye)],
        [z[0], z[1], z[2], -dot(z, eye)],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

/// 透视投影矩阵(视空间 → 裁剪空间)。`fov_y` 为垂直视场角(弧度),
/// `aspect = width / height`;**z 映射到 [0, 1]**(WebGPU),w = -z_view。
/// `near` > 0,`far` > `near`。
pub fn perspective(fov_y: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
    // f = 1/tan(半视场角):fov_y 是**完整**垂直视场角,取一半再求切。
    let f = 1.0 / (fov_y * 0.5).tan();
    [
        [f / aspect, 0.0, 0.0, 0.0],
        [0.0, f, 0.0, 0.0],
        [0.0, 0.0, far / (near - far), near * far / (near - far)],
        [0.0, 0.0, -1.0, 0.0],
    ]
}

/// 一台由 look_at + 透视参数完全确定的相机;渲染目标尺寸随相机走,
/// 保证纵横比一致(`gpu::HeadlessRenderer::render_mesh_view` 直接以
/// `width`/`height` 建目标)。
#[derive(Debug, Clone, PartialEq)]
pub struct Camera {
    pub view: Mat4,
    pub projection: Mat4,
    pub width: u32,
    pub height: u32,
    pub near: f32,
    pub far: f32,
}

impl Camera {
    /// `fov_y_deg` 垂直视场角(度),`near`/`far` 深度范围(near > 0 且 far > near)。
    /// `up` 不必与视线正交;共线时自动换参考轴。
    // 相机参数天然 8 元(图形 API 惯例,wgpu/winit 同款签名)。
    #[allow(clippy::too_many_arguments)]
    pub fn look_at(
        eye: [f32; 3],
        target: [f32; 3],
        up: [f32; 3],
        fov_y_deg: f32,
        width: u32,
        height: u32,
        near: f32,
        far: f32,
    ) -> Self {
        assert!(
            fov_y_deg > 0.0 && fov_y_deg < 180.0,
            "fov_y_deg must be in (0, 180), got {fov_y_deg}"
        );
        assert!(near > 0.0 && far > near, "need 0 < near < far");
        assert!(width > 0 && height > 0, "viewport must be non-empty");
        Self {
            view: view_look_at(eye, target, up),
            projection: perspective(
                fov_y_deg.to_radians(),
                width as f32 / height as f32,
                near,
                far,
            ),
            width,
            height,
            near,
            far,
        }
    }

    /// `projection * view`(一次乘好,shader 里只吃这一个矩阵)。
    pub fn view_proj(&self) -> Mat4 {
        mul(&self.projection, &self.view)
    }

    /// 世界点 → NDC `[x, y, z]`(z ∈ [0,1])。w ≤ 0(点在相机侧/位于镜头平面)
    /// 或结果非有限时返回 `None` —— 调用方据此剔除,GPU 侧裁剪是另一道闸。
    pub fn project(&self, p: [f32; 3]) -> Option<[f32; 3]> {
        let c = transform(&self.view_proj(), p);
        // w ≤ 0 或 NaN 时剔除(NaN 走 all-finite 闸)。
        if c[3] <= 0.0 || !c.iter().all(|v| v.is_finite()) {
            return None;
        }
        Some([c[0] / c[3], c[1] / c[3], c[2] / c[3]])
    }

    /// NDC `[x, y]`(∈ [-1,1],y 向上)→ 像素坐标(原点左上、y 向下,
    /// 连续坐标:像素中心在 i+0.5)。
    pub fn ndc_to_pixel(&self, ndc: [f32; 2]) -> [f32; 2] {
        [
            (ndc[0] + 1.0) * 0.5 * self.width as f32,
            (1.0 - ndc[1]) * 0.5 * self.height as f32,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 64×64,fov 90°(tan = 1),near 0.1 / far 100,相机在原点看 -z。
    fn camera() -> Camera {
        Camera::look_at(
            [0.0; 3],
            [0.0, 0.0, -1.0],
            [0.0, 1.0, 0.0],
            90.0,
            64,
            64,
            0.1,
            100.0,
        )
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4 * b.abs().max(1.0)
    }

    #[test]
    fn view_axes_follow_gl_convention() {
        // eye 在 +z 看向原点:原点在视空间 (0,0,-5);世界 +x 是屏幕右(+x),
        // 世界 +y 是屏幕上(+y);三行正交归一。
        let cam = Camera::look_at(
            [0.0, 0.0, 5.0],
            [0.0; 3],
            [0.0, 1.0, 0.0],
            60.0,
            8,
            8,
            0.1,
            10.0,
        );
        let at = |p| {
            let c = transform(&cam.view, p);
            [c[0], c[1], c[2]]
        };
        let o = at([0.0; 3]);
        assert!(
            close(o[0], 0.0) && close(o[1], 0.0) && close(o[2], -5.0),
            "{o:?}"
        );
        let r = at([1.0, 0.0, 5.0]);
        assert!(close(r[0], 1.0), "world +x must map to view +x: {r:?}");
        let u = at([0.0, 1.0, 5.0]);
        assert!(close(u[1], 1.0), "world +y must map to view +y: {u:?}");
        for row in &cam.view[..3] {
            let len = row[0] * row[0] + row[1] * row[1] + row[2] * row[2];
            assert!(close(len, 1.0));
        }
    }

    #[test]
    fn looking_straight_down_the_up_axis_stays_finite() {
        let cam = Camera::look_at(
            [0.0, 5.0, 0.0],
            [0.0; 3],
            [0.0, 1.0, 0.0],
            45.0,
            8,
            8,
            0.1,
            10.0,
        );
        let o = cam.project([0.0; 3]).expect("target ahead");
        assert!(close(o[0], 0.0) && close(o[1], 0.0));
    }

    #[test]
    fn straight_ahead_is_ndc_center_and_pixel_center() {
        let cam = camera();
        let p = cam.project([0.0, 0.0, -4.0]).unwrap();
        assert!(close(p[0], 0.0) && close(p[1], 0.0));
        let px = cam.ndc_to_pixel([p[0], p[1]]);
        assert!(close(px[0], 32.0) && close(px[1], 32.0));
    }

    #[test]
    fn near_and_far_planes_map_to_zero_and_one() {
        let cam = camera();
        let at_near = cam.project([0.0, 0.0, -cam.near]).unwrap();
        let at_far = cam.project([0.0, 0.0, -cam.far]).unwrap();
        assert!(close(at_near[2], 0.0), "near -> z_ndc 0: {}", at_near[2]);
        assert!(close(at_far[2], 1.0), "far -> z_ndc 1: {}", at_far[2]);
    }

    #[test]
    fn depth_is_nonlinear_and_concentrates_near_the_camera() {
        // 中点深度映射到 far/(far+near) ≈ 0.999:z ∈ [0,1] 的精度大头留给近处。
        let cam = camera();
        let mid = cam.project([0.0, 0.0, -(0.1 + 100.0) / 2.0]).unwrap();
        let want = 100.0 / (100.0 + 0.1);
        assert!(close(mid[2], want), "{} vs {want}", mid[2]);
        // 近处 1 单位深度的 z_ndc 变化远大于远处 1 单位(z_ndc 随深度单调增,取幅度)。
        let near_delta = (cam.project([0.0, 0.0, -2.0]).unwrap()[2]
            - cam.project([0.0, 0.0, -1.0]).unwrap()[2])
            .abs();
        let far_delta = (cam.project([0.0, 0.0, -51.0]).unwrap()[2]
            - cam.project([0.0, 0.0, -50.0]).unwrap()[2])
            .abs();
        assert!(near_delta > far_delta * 50.0);
    }

    #[test]
    fn frustum_edges_map_to_unit_ndc() {
        let cam = camera();
        // fov 90° → tan(45°)=1:深度 d 处的视锥半高/半宽都是 d。
        let up = cam.project([0.0, 4.0, -4.0]).unwrap();
        let right = cam.project([4.0, 0.0, -4.0]).unwrap();
        assert!(close(up[1], 1.0), "top edge -> y_ndc +1: {}", up[1]);
        assert!(close(right[0], 1.0), "right edge -> x_ndc +1: {}", right[0]);
    }

    #[test]
    fn aspect_ratio_widens_the_horizontal_fov() {
        // 128×64(.aspect 2)在深度 4 处:半宽 8 落在 x_ndc +1。
        let cam = Camera::look_at(
            [0.0; 3],
            [0.0, 0.0, -1.0],
            [0.0, 1.0, 0.0],
            90.0,
            128,
            64,
            0.1,
            100.0,
        );
        let edge = cam.project([8.0, 0.0, -4.0]).unwrap();
        assert!(close(edge[0], 1.0), "{}", edge[0]);
        // 垂直方向不随 aspect 变。
        let up = cam.project([0.0, 4.0, -4.0]).unwrap();
        assert!(close(up[1], 1.0));
    }

    #[test]
    fn behind_the_camera_is_not_projectable() {
        let cam = camera();
        assert_eq!(cam.project([0.0, 0.0, 1.0]), None, "z_view > 0 → w < 0");
        assert_eq!(cam.project([0.0, 0.0, 0.0]), None, "eye 平面 w = 0");
    }

    #[test]
    fn ndc_to_pixel_has_top_left_origin() {
        let cam = camera();
        assert!(close(cam.ndc_to_pixel([-1.0, 1.0])[0], 0.0), "x=-1 → 左");
        assert!(close(cam.ndc_to_pixel([-1.0, 1.0])[1], 0.0), "y=+1 → 上");
        assert!(close(cam.ndc_to_pixel([1.0, -1.0])[0], 64.0), "x=+1 → 右");
        assert!(close(cam.ndc_to_pixel([1.0, -1.0])[1], 64.0), "y=-1 → 下");
    }

    #[test]
    fn matrix_multiply_composes_transforms() {
        // 平移 T 与缩放 S:T*S*p == T 作用在 S*p 上。
        let t = [
            [1.0, 0.0, 0.0, 2.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let s = [
            [3.0, 0.0, 0.0, 0.0],
            [0.0, 3.0, 0.0, 0.0],
            [0.0, 0.0, 3.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let ts = mul(&t, &s);
        let p = transform(&ts, [1.0, 0.0, 0.0]);
        assert_eq!(p, [5.0, 0.0, 0.0, 1.0]);
        let st = mul(&s, &t);
        assert_eq!(transform(&st, [1.0, 0.0, 0.0]), [9.0, 0.0, 0.0, 1.0]);
    }
}
