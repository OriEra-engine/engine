//! 确定性前到背排序。
//!
//! 键 = 视空间深度 z 的 IEEE-754 位型(高 32 位)| splat 下标(低 32 位)。z > 0
//! (近平面已剔除)时位型随值单调递增,下标又使键两两不同——任何排序算法(含不稳定的
//! `sort_unstable`)都得到同一全序:升序 = 前到背,等深度按下标破平局。
//! 键打包思想借鉴 World Labs Spark/Radix 公开文档(深度位 + 索引位);
//! GPU LSD radix 版随 M1/M3 落地时沿用同一键格式。

use crate::project::ProjectedSplat;

/// 排序键。`depth` 必须为正(由 [`crate::project`] 的近平面剔除保证)。
pub fn sort_key(depth: f32, index: u32) -> u64 {
    debug_assert!(
        depth >= 0.0,
        "sort_key needs a non-negative depth, got {depth}"
    );
    (u64::from(depth.to_bits()) << 32) | u64::from(index)
}

/// 前到背的合成顺序:返回 `projected` 内的位置序列。
pub fn depth_order(projected: &[ProjectedSplat]) -> Vec<u32> {
    let mut keyed: Vec<(u64, u32)> = projected
        .iter()
        .enumerate()
        .map(|(slot, p)| (sort_key(p.depth, p.index), slot as u32))
        .collect();
    keyed.sort_unstable_by_key(|&(key, _)| key);
    keyed.into_iter().map(|(_, slot)| slot).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn splat(index: u32, depth: f32) -> ProjectedSplat {
        ProjectedSplat {
            index,
            depth,
            mean: [0.0; 2],
            conic: [1.0, 0.0, 1.0],
            extent: [1.0; 2],
            color: [1.0; 3],
            opacity: 1.0,
        }
    }

    #[test]
    fn keys_order_by_depth_then_index() {
        assert!(sort_key(1.0, 9) < sort_key(2.0, 0));
        assert!(sort_key(1.0, 1) < sort_key(1.0, 2));
        assert!(sort_key(0.25, u32::MAX) < sort_key(0.250_001, 0));
    }

    #[test]
    fn empty_input_yields_empty_order() {
        assert!(depth_order(&[]).is_empty());
    }

    #[test]
    fn back_to_front_input_is_reordered_front_to_back() {
        let p: Vec<_> = (0..5).map(|i| splat(i, 5.0 - i as f32)).collect();
        assert_eq!(depth_order(&p), [4, 3, 2, 1, 0]);
    }

    #[test]
    fn equal_depths_break_ties_by_original_index() {
        let p = [splat(7, 2.0), splat(3, 2.0), splat(5, 2.0)];
        assert_eq!(depth_order(&p), [1, 2, 0]);
    }

    #[test]
    fn matches_a_reference_comparison_sort() {
        // LCG 生成 2000 个深度,量化到 1/8 制造大量平局。
        let mut state = 0x5eed_u64;
        let p: Vec<_> = (0..2000)
            .map(|i| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let depth = 0.25 + ((state >> 40) % 400) as f32 / 8.0;
                splat(1999 - i, depth)
            })
            .collect();
        let mut want: Vec<u32> = (0..p.len() as u32).collect();
        want.sort_by(|&a, &b| {
            let (a, b) = (&p[a as usize], &p[b as usize]);
            a.depth.total_cmp(&b.depth).then(a.index.cmp(&b.index))
        });
        assert_eq!(depth_order(&p), want);
    }
}
