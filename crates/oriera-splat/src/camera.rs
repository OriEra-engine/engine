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
}
