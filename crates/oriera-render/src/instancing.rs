//! L5 实例化合批(feature `instancing`,默认关)——真实场景批次 WP4。
//!
//! CPU 合批规划器:把一列(已剔除的)场景实例按 (mesh, material) 分组,
//! 产出 [`crate::scene::DrawBatch`] 列表。这是 GPU 侧 `DrawList`(RFC 0004
//! 步 1a,PR #207)的 CPU 前身:render-wgpu 持久路径落地之前,先在 CPU 上
//! 把"同网格同材质"的实例排到一起;将来 GPU 侧直接吃这里的输出——**逐批、
//! 按每批 `instances` 的下标顺序 push 进 DrawList,即得同一绘制序列**(与
//! [`crate::scene`] 模块文档的约定一致)。[`batch_by_key`] 与
//! [`batch_by_key_sorted`] 分出的组与组内容完全相同,只差组间顺序,因此
//! 接哪个变体进 DrawList 都自洽,只是批序策略不同。
//!
//! **契约**
//! - 纯 CPU、headless 可测,零新增依赖;输入输出只经过 [`crate::scene`]
//!   词汇:吃 `&[SceneInstance]`,产 `Vec<DrawBatch>`。实例的矩阵与包围球
//!   原样透传,本模块不做任何变换、也不丢实例——每个输入下标恰好出现在
//!   一个批里(有测试钉住这条不变量)。
//! - [`batch_by_key`]:组序 = 键首次出现顺序(保场景发射序)。键序用 Vec
//!   记录,HashMap 只做"键 → 组下标"查找、**绝不迭代**——哈希迭代序不可靠,
//!   输出与哈希实现无关。
//! - [`batch_by_key_sorted`]:组序 = (mesh, material) 字典序(BTreeMap)。
//!   两个变体的差别与各自用途见 [`batch_by_key_sorted`] 文档。
//! - 批内实例下标一律升序:单趟按下标顺序扫输入,自然得到。
//! - **确定性**:无随机、无哈希迭代序依赖;同输入两次跑,输出逐位一致。
//!
//! **诚实边界**
//! - 只按相等性分组:L5 不解释 mesh / material 的含义(它们是调用方命名
//!   空间里的 id),没有材质排序键,也不按深度/状态排序。
//! - 不做剔除(那是 `cull` 的事,本模块不 use 它)、不做实例层级(父子
//!   变换)、不做 GPU 侧 instance buffer 打包(那是 DrawList 的事)。
//! - 实例下标是 u32(`crate::scene::DrawBatch` 词汇所定),输入超过 2^32
//!   个实例不在适用范围。
//! - [`stats`] 只描述传入的这一份批列表,不缓存、不跨帧累积。

use std::collections::{BTreeMap, HashMap};

use crate::scene::{DrawBatch, SceneInstance};

/// 按 (mesh, material) 分组,组序 = 键**首次出现**的顺序。
///
/// 哪个键先在实例列表里露头,哪组就排前面——保持调用方的场景发射序,
/// 适合与剔除输出顺序对齐、以及任何"按场景出现先后画"的场合。HashMap
/// 只做查找、绝不被迭代,组间顺序完全由首次出现决定,与哈希实现无关。
/// 批内实例下标升序;同输入两次跑,输出逐位一致。
pub fn batch_by_key(insts: &[SceneInstance]) -> Vec<DrawBatch> {
    // 键序与组内容按下标对齐、同步 push;`order` 是唯一的组序来源。
    let mut order: Vec<(u32, u32)> = Vec::new();
    let mut groups: Vec<Vec<u32>> = Vec::new();
    // 只查不迭代:HashMap 在这里纯粹是 O(1) 摊还的"键 → 组下标"查找表。
    let mut lookup: HashMap<(u32, u32), usize> = HashMap::new();
    for (i, inst) in insts.iter().enumerate() {
        let key = (inst.mesh, inst.material);
        let gi = match lookup.get(&key) {
            Some(&gi) => gi,
            None => {
                let gi = groups.len();
                lookup.insert(key, gi);
                order.push(key);
                groups.push(Vec::new());
                gi
            }
        };
        groups[gi].push(i as u32);
    }
    order
        .into_iter()
        .zip(groups)
        .map(|((mesh, material), instances)| DrawBatch {
            mesh,
            material,
            instances,
        })
        .collect()
}

/// [`batch_by_key`] 的字典序变体:组序 = (mesh, material) 升序(BTreeMap)。
///
/// 与首次出现序的差别:首次出现序**跟随输入排列**——同一批实例换个顺序
/// 喂进来,组间顺序就变;字典序对同一实例集合的任意排列给出同一批序,
/// 是**规范序**。用途划分:要保场景发射序(与剔除/动画的时序对齐)用
/// [`batch_by_key`];要黄金文件对比、跨端回放、或上游顺序无保证时仍可
/// 复现批序,用本函数。两者分组与组内容完全相同,只差组间顺序;批内
/// 实例下标同样升序,同输入两次跑逐位一致。
pub fn batch_by_key_sorted(insts: &[SceneInstance]) -> Vec<DrawBatch> {
    // BTreeMap 的迭代序 = 键序,本身就是确定的全序;批内仍按下标升序 append。
    let mut groups: BTreeMap<(u32, u32), Vec<u32>> = BTreeMap::new();
    for (i, inst) in insts.iter().enumerate() {
        groups
            .entry((inst.mesh, inst.material))
            .or_default()
            .push(i as u32);
    }
    groups
        .into_iter()
        .map(|((mesh, material), instances)| DrawBatch {
            mesh,
            material,
            instances,
        })
        .collect()
}

/// 一个批列表的粗略体检:批数、实例总数、最大批、单实例批。
///
/// `batches` ≈ 逐批一次 draw 的 draw call 数;`instances` 应等于输入实例
/// 数(合批不丢实例,可当断言用);`max_batch` 是单次 draw 摊到的最多
/// 实例(合批效果的上界);`singletons` 是没赚到合批的批数——偏多说明键
/// 太散,值得上层看看是资产碎片还是剔除太狠。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BatchStats {
    /// 批数量。
    pub batches: usize,
    /// 所有批的实例总数。
    pub instances: usize,
    /// 最大批的实例数(空列表为 0)。
    pub max_batch: usize,
    /// 只含一个实例的批数。
    pub singletons: usize,
}

/// 统计一个批列表(见 [`BatchStats`]);空列表得全零。
pub fn stats(batches: &[DrawBatch]) -> BatchStats {
    let mut out = BatchStats {
        batches: batches.len(),
        ..BatchStats::default()
    };
    for b in batches {
        out.instances += b.instances.len();
        out.max_batch = out.max_batch.max(b.instances.len());
        if b.instances.len() == 1 {
            out.singletons += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::Mat4;
    use crate::scene::Bounds;

    fn ident() -> Mat4 {
        let mut m = [[0.0; 4]; 4];
        for (i, row) in m.iter_mut().enumerate() {
            row[i] = 1.0;
        }
        m
    }

    /// 造一个实例:矩阵与包围球跟分组无关,单位矩阵 + 空包围球即可。
    fn inst(mesh: u32, material: u32) -> SceneInstance {
        SceneInstance::new(mesh, material, ident(), Bounds::EMPTY)
    }

    fn keys_of(batches: &[DrawBatch]) -> Vec<(u32, u32)> {
        batches.iter().map(|b| (b.mesh, b.material)).collect()
    }

    /// 批序形状:每批的 (键, 实例数)——对输入排列不变的那部分。
    fn shape(batches: &[DrawBatch]) -> Vec<((u32, u32), usize)> {
        batches
            .iter()
            .map(|b| ((b.mesh, b.material), b.instances.len()))
            .collect()
    }

    /// 不变量:批非空;批的 mesh/material 与它引用的实例一致;批内下标
    /// 严格升序;每个输入下标恰好被一个批引用(合批不丢、不重)。
    fn assert_invariants(insts: &[SceneInstance], batches: &[DrawBatch]) {
        let mut seen = vec![false; insts.len()];
        for b in batches {
            assert!(!b.instances.is_empty(), "不该有空批: {b:?}");
            let mut prev: Option<u32> = None;
            for &i in &b.instances {
                let inst = &insts[i as usize];
                assert_eq!(
                    (inst.mesh, inst.material),
                    (b.mesh, b.material),
                    "批 {b:?} 引用了不同键的实例"
                );
                assert!(!seen[i as usize], "实例 {i} 被多个批引用");
                seen[i as usize] = true;
                if let Some(p) = prev {
                    assert!(p < i, "批内下标必须升序: {p} 后跟 {i}");
                }
                prev = Some(i);
            }
        }
        assert!(
            seen.iter().all(|&s| s),
            "有实例没进任何批: {:?}",
            seen.iter()
                .enumerate()
                .filter(|(_, &s)| !s)
                .map(|(i, _)| i)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn all_same_key_collapses_to_one_batch() {
        let insts: Vec<_> = (0..8).map(|_| inst(3, 5)).collect();
        let b = batch_by_key(&insts);
        assert_eq!(b.len(), 1);
        assert_eq!((b[0].mesh, b[0].material), (3, 5));
        assert_eq!(b[0].instances, (0..8).collect::<Vec<u32>>());
        // 只有一组时两个变体没有任何差别。
        assert_eq!(batch_by_key_sorted(&insts), b);
        let st = stats(&b);
        assert_eq!(
            st,
            BatchStats {
                batches: 1,
                instances: 8,
                max_batch: 8,
                singletons: 0
            }
        );
    }

    #[test]
    fn all_distinct_keys_are_singletons() {
        let insts: Vec<_> = (0..5).map(|i| inst(i + 10, i * 2)).collect();
        let b = batch_by_key(&insts);
        let s = batch_by_key_sorted(&insts);
        for batches in [&b, &s] {
            assert_eq!(batches.len(), 5, "全异键 → N 个 singleton");
            for (i, batch) in batches.iter().enumerate() {
                assert_eq!(batch.instances, vec![i as u32]);
            }
            assert_eq!(
                stats(batches),
                BatchStats {
                    batches: 5,
                    instances: 5,
                    max_batch: 1,
                    singletons: 5
                }
            );
        }
        assert_invariants(&insts, &b);
        assert_invariants(&insts, &s);
    }

    #[test]
    fn group_order_follows_first_appearance() {
        // (9,0) 先露头、(1,0) 其次——字典序会把它们反过来;首现序不看键值。
        let insts = vec![inst(9, 0), inst(1, 0), inst(9, 0), inst(5, 5)];
        let b = batch_by_key(&insts);
        assert_eq!(keys_of(&b), vec![(9, 0), (1, 0), (5, 5)]);
        assert_eq!(b[0].instances, vec![0, 2]);
        assert_eq!(b[1].instances, vec![1]);
        assert_eq!(b[2].instances, vec![3]);
        // 字典序变体:同样的组与组内容,顺序按 (mesh, material) 升序。
        let s = batch_by_key_sorted(&insts);
        assert_eq!(keys_of(&s), vec![(1, 0), (5, 5), (9, 0)]);
        // 组内容做无序比较(键 → 实例数),组间顺序已在上面各自断言。
        let as_map = |batches: &[DrawBatch]| -> BTreeMap<(u32, u32), usize> {
            batches
                .iter()
                .map(|b| ((b.mesh, b.material), b.instances.len()))
                .collect()
        };
        assert_eq!(as_map(&s), as_map(&b), "组与组内容相同,只差组间顺序");
        assert_invariants(&insts, &b);
        assert_invariants(&insts, &s);
    }

    #[test]
    fn shuffled_interleaving_keeps_ascending_indices() {
        // A B A C B A:三组交错出现,批内下标必须仍升序。
        let insts = vec![
            inst(1, 1),
            inst(2, 2),
            inst(1, 1),
            inst(3, 3),
            inst(2, 2),
            inst(1, 1),
        ];
        let b = batch_by_key(&insts);
        assert_eq!(keys_of(&b), vec![(1, 1), (2, 2), (3, 3)]);
        assert_eq!(b[0].instances, vec![0, 2, 5]);
        assert_eq!(b[1].instances, vec![1, 4]);
        assert_eq!(b[2].instances, vec![3]);
        assert_invariants(&insts, &b);
        assert_invariants(&insts, &batch_by_key_sorted(&insts));
    }

    #[test]
    fn empty_input_yields_empty_output() {
        assert!(batch_by_key(&[]).is_empty());
        assert!(batch_by_key_sorted(&[]).is_empty());
        assert_eq!(
            stats(&[]),
            BatchStats {
                batches: 0,
                instances: 0,
                max_batch: 0,
                singletons: 0
            }
        );
        assert_eq!(stats(&[]), BatchStats::default());
    }

    #[test]
    fn stats_numbers_are_exact() {
        // A A B A C C:A=[0,1,3]、B=[2]、C=[4,5]。
        let insts = vec![
            inst(1, 0),
            inst(1, 0),
            inst(2, 0),
            inst(1, 0),
            inst(3, 7),
            inst(3, 7),
        ];
        let b = batch_by_key(&insts);
        assert_eq!(keys_of(&b), vec![(1, 0), (2, 0), (3, 7)]);
        let st = stats(&b);
        assert_eq!(st.batches, 3);
        assert_eq!(st.instances, 6);
        assert_eq!(st.max_batch, 3);
        assert_eq!(st.singletons, 1);
        // 统计对两个变体一致(它们只差组间顺序)。
        assert_eq!(stats(&batch_by_key_sorted(&insts)), st);
        assert_invariants(&insts, &b);
    }

    #[test]
    fn double_run_is_bit_identical() {
        let insts: Vec<_> = (0..24).map(|i| inst((i * 7) % 4, (i * 5) % 3)).collect();
        let a = batch_by_key(&insts);
        let b = batch_by_key(&insts);
        assert_eq!(a, b, "同输入两次跑必须逐位一致");
        let sa = batch_by_key_sorted(&insts);
        let sb = batch_by_key_sorted(&insts);
        assert_eq!(sa, sb);
        // 统计同样可复现,且两个变体的统计一致。
        let st = stats(&a);
        assert_eq!(stats(&b), st);
        assert_eq!(stats(&sa), st);
        assert_eq!(stats(&sb), st);
        assert_invariants(&insts, &a);
        assert_invariants(&insts, &sa);
    }

    #[test]
    fn sorted_variant_is_invariant_under_input_permutation() {
        let base: Vec<_> = (0..12).map(|i| inst((i * 3) % 5, (i * 7) % 2)).collect();
        // 同一实例集合的另一种排列。
        let perm: Vec<_> = [11usize, 3, 7, 0, 9, 1, 5, 10, 2, 8, 4, 6]
            .iter()
            .map(|&i| base[i])
            .collect();
        let canonical = shape(&batch_by_key_sorted(&base));
        assert_eq!(
            shape(&batch_by_key_sorted(&perm)),
            canonical,
            "字典序对任意输入排列给出同一批序(规范序)"
        );
        // 首现序跟随排列——选定的两种排列下批序确实不同,钉住两变体的文档差别。
        assert_ne!(keys_of(&batch_by_key(&base)), keys_of(&batch_by_key(&perm)));
        assert_invariants(&base, &batch_by_key_sorted(&base));
        assert_invariants(&perm, &batch_by_key_sorted(&perm));
    }
}
