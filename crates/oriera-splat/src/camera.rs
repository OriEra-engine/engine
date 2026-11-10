//! splat 专用针孔相机。约定同 OpenCV/COLMAP:相机系 x 右、y 下、z 前(z > 0 在镜头前);
//! 像素 (i, j) 覆盖连续坐标 [i, i+1) × [j, j+1),中心在 (i+½, j+½)。
//!
//! L5 通用相机矩阵随 #152(原生 glTF+PBR)落地前,这是泼溅路径自带的最小相机;
//! 两者统一时以 #152 为准。

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SplatCamera {
    /// 世界→相机旋转,行主序:三行依次是相机 x/y/z 轴在世界系中的方向。
    pub rotation: [[f32; 3]; 3],
    /// 世界→相机平移:`p_cam = rotation · p_world + translation`。
    pub translation: [f32; 3],
    /// 焦距(像素)。
    pub fx: f32,
    pub fy: f32,
    /// 主点(像素,连续坐标)。
    pub cx: f32,
    pub cy: f32,
    pub width: u32,
    pub height: u32,
}

impl SplatCamera {
    /// 从 `eye` 看向 `target`;`up` 为世界上方向(不必与视线正交),`fov_y_deg` 为垂直视场角。
    /// 主点取画面中心,方形像素(fx = fy)。视线与 `up` 平行时换一根不共线的参考轴。
    pub fn look_at(
        eye: [f32; 3],
        target: [f32; 3],
        up: [f32; 3],
        fov_y_deg: f32,
        width: u32,
        height: u32,
    ) -> Self {
        let z = normalize(sub(target, eye));
        let mut x = cross(z, up);
        if dot(x, x) < 1e-12 {
            let alt = if z[1].abs() < 0.9 {
                [0.0, 1.0, 0.0]
            } else {
                [1.0, 0.0, 0.0]
            };
            x = cross(z, alt);
        }
        let x = normalize(x);
        let y = cross(z, x);
        let f = height as f32 * 0.5 / (fov_y_deg.to_radians() * 0.5).tan();
        Self {
            rotation: [x, y, z],
            translation: [-dot(x, eye), -dot(y, eye), -dot(z, eye)],
            fx: f,
            fy: f,
            cx: width as f32 * 0.5,
            cy: height as f32 * 0.5,
            width,
            height,
        }
    }

    /// 世界点 → 相机系。
    pub fn to_camera(&self, p: [f32; 3]) -> [f32; 3] {
        let r = &self.rotation;
        [
            dot(r[0], p) + self.translation[0],
            dot(r[1], p) + self.translation[1],
            dot(r[2], p) + self.translation[2],
        ]
    }

    /// 水平/垂直半视场角正切(EWA Jacobian 钳制用)。
    pub fn tan_half_fov(&self) -> (f32, f32) {
        (
            self.width as f32 * 0.5 / self.fx,
            self.height as f32 * 0.5 / self.fy,
        )
    }
}

#[cfg(feature = "splat-wgpu")]
impl SplatCamera {
    /// 从 #152 共享底座的 GL 约定相机（[`oriera_render::camera::Camera`]：
    /// y 向上、镜头看 -z）换算出泼溅路径的 OpenCV 约定相机（PLAN-splat M1）。
    /// 同一 eye/target/fov/内参，只有表示不同——视图矩阵行 [x, y, z_back]
    /// → [x, -y, -z_back]（y 向下、z 前向），平移 [t0, -t1, -t2]；**像素焦距**
    /// 从投影矩阵反解：矩阵系数是 NDC 归一化的 f=1/tan(fov/2)，泼溅相机要
    /// 像素量纲 fx = P[0][0]·width/2、fy = P[1][1]·height/2。
    /// 换算结果用闭式单测钉死（`from_view_matches_gl_camera_axis_for_axis`）。
    pub fn from_view(cam: &oriera_render::camera::Camera) -> Self {
        let negate = |v: [f32; 3]| [-v[0], -v[1], -v[2]];
        let row = |r: usize| [cam.view[r][0], cam.view[r][1], cam.view[r][2]];
        Self {
            rotation: [row(0), negate(row(1)), negate(row(2))],
            translation: [cam.view[0][3], -cam.view[1][3], -cam.view[2][3]],
            fx: cam.projection[0][0] * cam.width as f32 * 0.5,
            fy: cam.projection[1][1] * cam.height as f32 * 0.5,
            cx: cam.width as f32 * 0.5,
            cy: cam.height as f32 * 0.5,
            width: cam.width,
            height: cam.height,
        }
    }
}

pub(crate) fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = dot(v, v).sqrt();
    [v[0] / len, v[1] / len, v[2] / len]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5)
    }

    #[test]
    fn look_at_maps_target_to_principal_point_with_opencv_axes() {
        let cam = SplatCamera::look_at([0.0, 0.0, 5.0], [0.0; 3], [0.0, 1.0, 0.0], 60.0, 64, 48);
        // 目标在正前方 5 个单位
        assert!(near(cam.to_camera([0.0; 3]), [0.0, 0.0, 5.0]));
        // 世界 +X 在画面右侧(相机 x > 0),世界 +Y 在画面上方(相机 y < 0)
        assert!(cam.to_camera([1.0, 0.0, 0.0])[0] > 0.0);
        assert!(cam.to_camera([0.0, 1.0, 0.0])[1] < 0.0);
        assert_eq!((cam.cx, cam.cy), (32.0, 24.0));
        let (_, tan_y) = cam.tan_half_fov();
        assert!((tan_y - 30f32.to_radians().tan()).abs() < 1e-6);
    }

    #[test]
    fn rotation_rows_are_orthonormal_and_right_handed() {
        let cam = SplatCamera::look_at(
            [3.0, 2.0, -4.0],
            [0.5, 0.0, 1.0],
            [0.0, 1.0, 0.0],
            45.0,
            8,
            8,
        );
        let [x, y, z] = cam.rotation;
        for (a, b) in [(x, y), (y, z), (z, x)] {
            assert!(dot(a, b).abs() < 1e-6);
        }
        for v in [x, y, z] {
            assert!((dot(v, v) - 1.0).abs() < 1e-6);
        }
        assert!(near(cross(x, y), z));
    }

    #[test]
    fn looking_straight_down_the_up_axis_stays_finite() {
        let cam = SplatCamera::look_at([0.0, 5.0, 0.0], [0.0; 3], [0.0, 1.0, 0.0], 45.0, 8, 8);
        assert!(cam.rotation.iter().flatten().all(|v| v.is_finite()));
        assert!(near(cam.to_camera([0.0; 3]), [0.0, 0.0, 5.0]));
    }

    #[cfg(feature = "splat-wgpu")]
    #[test]
    fn from_view_matches_gl_camera_axis_for_axis() {
        use oriera_render::camera::{self, Camera};
        let gl = Camera::look_at(
            [0.0, 0.0, 5.0],
            [0.0; 3],
            [0.0, 1.0, 0.0],
            60.0,
            64,
            48,
            0.1,
            100.0,
        );
        let cv = SplatCamera::from_view(&gl);
        // 内参:像素焦距 = 矩阵系数 × 维度/2(NDC 归一化 → 像素量纲)
        assert_eq!(cv.fx, gl.projection[0][0] * 32.0);
        assert_eq!(cv.fy, gl.projection[1][1] * 24.0);
        assert_eq!((cv.cx, cv.cy, cv.width, cv.height), (32.0, 24.0, 64, 48));
        // 轴约定:GL x=右/ y=上 /z=后 ↔ OpenCV x=右 /y=下 /z=前
        for p in [[0.0f32; 3], [0.3, -0.4, 5.0], [-1.0, 2.0, 4.0]] {
            let g = camera::transform(&gl.view, p);
            let c = cv.to_camera(p);
            assert!((g[0] - c[0]).abs() < 1e-5, "x {p:?}: {g:?} vs {c:?}");
            assert!((g[1] + c[1]).abs() < 1e-5, "y {p:?}: {g:?} vs {c:?}");
            assert!((g[2] + c[2]).abs() < 1e-5, "z {p:?}: {g:?} vs {c:?}");
        }
        // 同一点在两种约定下落在同一像素
        let p = [0.3f32, -0.4, 3.0];
        let ndc = gl.project(p).unwrap();
        let px_gl = gl.ndc_to_pixel([ndc[0], ndc[1]]);
        let c = cv.to_camera(p);
        let px_cv = [cv.fx * c[0] / c[2] + cv.cx, cv.fy * c[1] / c[2] + cv.cy];
        assert!((px_gl[0] - px_cv[0]).abs() < 1e-3 && (px_gl[1] - px_cv[1]).abs() < 1e-3);
    }
}
