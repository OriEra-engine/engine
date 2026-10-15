//! L9 cognition loop — NPCs as engine systems on the L2 scheduler.
//!
//! This is what "AI embedded in the engine" means: talking, remembering
//! and world-progression are [`System<World>`]s that tick on the engine
//! loop like physics does. An application only (1) registers components
//! and resources, (2) adds the systems to the scheduler, (3) reads the
//! `DialogueLog`/`WorldStory` resources to render. No LLM calls exist
//! in application code — the cognition system goes through the L7
//! [`ModelProvider`] boundary.

use crate::memory::{Episode, MemoryStore};
use oriera_core::{FrameCtx, System};
use oriera_ecs::World;
use oriera_infer::{ChatMessage, ModelProvider, UsageMeter};
use std::sync::Arc;

/// NPC identity component (L3, registered by the application).
#[derive(Debug, Clone, PartialEq)]
pub struct Persona {
    pub name: String,
    pub role: String,
    pub description: String,
}

/// NPC mood component, written by cognition each speaking turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mood {
    Happy,
    Neutral,
    Sad,
    Excited,
    Thoughtful,
}

impl Mood {
    pub fn label(self) -> &'static str {
        match self {
            Mood::Happy => "happy",
            Mood::Neutral => "neutral",
            Mood::Sad => "sad",
            Mood::Excited => "excited",
            Mood::Thoughtful => "thoughtful",
        }
    }
}

/// One line of café dialogue (resource, L3).
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub speaker: String,
    pub to: String,
    pub text: String,
}

/// Rolling dialogue log resource.
#[derive(Debug, Default, Clone)]
pub struct DialogueLog {
    pub turns: Vec<Turn>,
}

impl DialogueLog {
    pub fn recent(&self, k: usize) -> &[Turn] {
        let start = self.turns.len().saturating_sub(k);
        &self.turns[start..]
    }
}

/// AI-director state: world day + current story beat (book T12).
#[derive(Debug, Default, Clone)]
pub struct WorldStory {
    pub day: u32,
    pub turn: u64,
    pub beats: Vec<String>,
}

impl WorldStory {
    pub fn current_beat(&self) -> &str {
        self.beats
            .last()
            .map(String::as_str)
            .unwrap_or("平静的日常")
    }
}

const DEFAULT_BEATS: &[&str] = &[
    "雨季来了，店里放起了爵士",
    "绘本画到了'会说话的咖啡机'这一章",
    "街对面开了连锁咖啡，老周嘴上不在意",
    "停电之夜：三个人点着蜡烛聊到打烊",
    "咖啡馆一周年展：墙上挂满客人的故事",
];

/// Cognition system: every `interval` ticks one NPC speaks, chosen
/// round-robin among entities that have a [`Persona`]. The reply is
/// produced through the L7 model boundary, memories are recorded, and
/// the dialogue log grows — all inside the engine tick.
pub struct CognitionSystem<P: ModelProvider> {
    provider: Arc<P>,
    /// Speak once per N engine ticks.
    pub interval: u32,
    /// Round-robin cursor over speaking NPCs.
    cursor: usize,
    /// Cached speaker order (rebuilt when the roster changes).
    order: Vec<oriera_ecs::Entity>,
}

impl<P: ModelProvider> CognitionSystem<P> {
    pub fn new(provider: Arc<P>, interval: u32) -> Self {
        Self {
            provider,
            interval: interval.max(1),
            cursor: 0,
            order: Vec::new(),
        }
    }

    fn roster(&mut self, world: &World) {
        self.order = world.query::<Persona>().map(|(e, _)| e).collect();
        if self.cursor >= self.order.len() {
            self.cursor = 0;
        }
    }
}

impl<P: ModelProvider> System<World> for CognitionSystem<P> {
    fn name(&self) -> &str {
        "l9-cognition"
    }

    fn run(&mut self, _ctx: &FrameCtx, world: &mut World) {
        let (turn, day, beat) = {
            let story = world.resource_or_default::<WorldStory>();
            story.turn += 1;
            (story.turn, story.day, story.current_beat().to_string())
        };

        if turn % self.interval as u64 != 0 {
            return;
        }
        self.roster(world);
        if self.order.is_empty() {
            return;
        }
        let speaker_e = self.order[self.cursor % self.order.len()];
        self.cursor += 1;

        let (persona, _mood) = match (
            world.get::<Persona>(speaker_e),
            world.get::<Mood>(speaker_e),
        ) {
            (Some(p), m) => (p.clone(), m.copied().unwrap_or(Mood::Neutral)),
            (None, _) => return,
        };

        // Pick a partner: next persona-bearing entity in rotation.
        let partner = self
            .order
            .iter()
            .cycle()
            .skip(self.cursor)
            .find_map(|e| {
                let p = world.get::<Persona>(*e)?;
                (p.name != persona.name).then(|| p.clone())
            })
            .unwrap_or_else(|| Persona {
                name: "客人".into(),
                role: "刚推门的旅行者".into(),
                description: String::new(),
            });

        let log = world
            .resource::<DialogueLog>()
            .cloned()
            .unwrap_or_default();
        let recent: Vec<String> = log
            .recent(6)
            .iter()
            .map(|t| format!("{}：{}", t.speaker, t.text))
            .collect();

        let mems = world
            .resource::<MemoryStore>()
            .map(|m| m.retrieve(&persona.name, &partner.name, &recent.join(""), turn, 4))
            .unwrap_or_default();
        let mem_lines: Vec<String> = mems
            .iter()
            .map(|m| format!("- (第{}日) {}", m.day, m.text))
            .collect();

        let sys = format!(
            "你是「{name}」，{role}，活在 OriEra 世界引擎的隅角咖啡馆。\n性格：{desc}\n\
             世界进度：第 {day} 世界日，{beat}\n对话对象：「{pname}」（{prole}）。\n\
             你的相关记忆：\n{mems}\n最近对话：\n{recent}\n\
             请以 {name} 的口吻说 1~2 句话，随后在同一行以 | 分隔给出 \
             动作(idle/wave/dance)与心情(happy/neutral/sad/excited/thoughtful)，\
             再用 | 给出一句≤20字的长期记忆要点。",
            name = persona.name,
            role = persona.role,
            desc = persona.description,
            day = day,
            beat = beat,
            pname = partner.name,
            prole = partner.role,
            mems = if mem_lines.is_empty() {
                "-（暂无）".into()
            } else {
                mem_lines.join("\n")
            },
            recent = if recent.is_empty() {
                "（对话刚开始）".into()
            } else {
                recent.join("\n")
            },
        );

        let reply = self
            .provider
            .complete(&[ChatMessage::system(sys), ChatMessage::user("（请接话）")])
            .unwrap_or_else(|_| "……".into());
        meter_usage(world, &*self.provider);

        let mut parts = reply.split('|').map(str::trim);
        let say = parts.next().unwrap_or("……").to_string();
        let action = parts.next().unwrap_or("idle").to_string();
        let mood = match parts.next().unwrap_or("neutral") {
            "happy" => Mood::Happy,
            "sad" => Mood::Sad,
            "excited" => Mood::Excited,
            "thoughtful" => Mood::Thoughtful,
            _ => Mood::Neutral,
        };
        let remember = parts.next().unwrap_or("").to_string();

        if let Some(m) = world.get_mut::<Mood>(speaker_e) {
            *m = mood;
        }
        world
            .resource_or_default::<DialogueLog>()
            .turns
            .push(Turn {
                speaker: persona.name.clone(),
                to: partner.name.clone(),
                text: say.clone(),
            });
        let log = world.resource_or_default::<DialogueLog>();
        if log.turns.len() > 24 {
            let drop = log.turns.len() - 24;
            log.turns.drain(0..drop);
        }
        if !remember.is_empty() {
            world.resource_or_default::<MemoryStore>().record(
                &persona.name,
                Episode {
                    turn,
                    day,
                    about: partner.name.clone(),
                    text: remember,
                    importance: 3,
                },
            );
        }
        // The partner also half-remembers the exchange.
        let head: String = say.chars().take(18).collect();
        world.resource_or_default::<MemoryStore>().record(
            &partner.name,
            Episode {
                turn,
                day,
                about: persona.name.clone(),
                text: format!("{}说了：{}", persona.name, head),
                importance: 2,
            },
        );
        let _ = action; // L6 animation mapping lands with the 3D runtime
    }
}

/// Record a provider call's token usage into the world's
/// [`UsageMeter`] — but only when the provider actually measured
/// something, so worlds without metering stay metering-free.
fn meter_usage<P: ModelProvider>(world: &mut World, provider: &P) {
    let usage = provider.last_usage();
    if usage.total() > 0 {
        world.resource_or_default::<UsageMeter>().record(usage);
    }
}

/// AI director: every `interval` cognition turns, advance the story.
pub struct DirectorSystem<P: ModelProvider> {
    provider: Arc<P>,
    pub interval: u64,
    beat_idx: usize,
}

impl<P: ModelProvider> DirectorSystem<P> {
    pub fn new(provider: Arc<P>, interval: u64) -> Self {
        Self {
            provider,
            interval: interval.max(1),
            beat_idx: 0,
        }
    }
}

impl<P: ModelProvider> System<World> for DirectorSystem<P> {
    fn name(&self) -> &str {
        "l9-director"
    }

    fn run(&mut self, _ctx: &FrameCtx, world: &mut World) {
        let (turn, day) = {
            let s = world.resource_or_default::<WorldStory>();
            s.turn += 1;
            (s.turn, s.day)
        };
        if turn % self.interval != 0 {
            return;
        }
        let log = world
            .resource::<DialogueLog>()
            .cloned()
            .unwrap_or_default();
        let sys = format!(
            "你是 OriEra 世界引擎的 AI 导演。根据最近对话推进一个小而具体的世界事件，只输出一句话。\n当前第 {} 世界日。最近对话：{}",
            day,
            log.recent(6)
                .iter()
                .map(|t| format!("{}：{}", t.speaker, t.text))
                .collect::<Vec<_>>()
                .join(" / ")
        );
        let beat = self
            .provider
            .complete(&[ChatMessage::system(sys)])
            .ok()
            .map(|s| s.trim().trim_matches('"').to_string())
            .filter(|s| !s.is_empty() && s.chars().count() < 60)
            .unwrap_or_else(|| {
                let b = DEFAULT_BEATS[self.beat_idx % DEFAULT_BEATS.len()].to_string();
                self.beat_idx += 1;
                b
            });
        meter_usage(world, &*self.provider);

        let story = world.resource_or_default::<WorldStory>();
        story.beats.push(beat.clone());
        if story.beats.len() > 30 {
            story.beats.drain(0..story.beats.len() - 30);
        }
        if turn % (self.interval * 3) == 0 {
            story.day += 1;
        }
        let mem = world.resource_or_default::<MemoryStore>();
        let agents: Vec<String> = mem.agents().map(str::to_string).collect();
        for agent in agents {
            mem.record(
                &agent,
                Episode {
                    turn,
                    day,
                    about: "world".into(),
                    text: format!("世界事件：{beat}"),
                    importance: 4,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oriera_common::{FrameIndex, Timestamp};
    use oriera_infer::RuleProvider;
    use std::sync::Arc;

    fn ctx() -> FrameCtx {
        FrameCtx {
            frame: FrameIndex(0),
            dt: 0.0,
            now: Timestamp(0),
        }
    }

    fn cafe_world() -> World {
        let mut w = World::new();
        w.insert_resource(DialogueLog::default());
        w.insert_resource(WorldStory {
            day: 1,
            turn: 0,
            beats: vec![],
        });
        w.insert_resource(MemoryStore::new());
        for (name, role, desc) in [
            ("老周", "老板", "手冲派，记得熟客口味"),
            ("Aria", "插画师", "在画会说话的咖啡馆"),
            ("阿哲", "学生", "想做世界引擎"),
        ] {
            let e = w.spawn();
            w.insert(
                e,
                Persona {
                    name: name.into(),
                    role: role.into(),
                    description: desc.into(),
                },
            );
            w.insert(e, Mood::Neutral);
        }
        w
    }

    #[test]
    fn cognition_drives_dialogue_on_engine_ticks() {
        let mut w = cafe_world();
        let mut cog = CognitionSystem::new(Arc::new(RuleProvider::new()), 1);
        let mut dir = DirectorSystem::new(Arc::new(RuleProvider::new()), 5);
        for _ in 0..12 {
            cog.run(&ctx(), &mut w);
            dir.run(&ctx(), &mut w);
        }
        let log = w.resource::<DialogueLog>().unwrap();
        assert!(
            log.turns.len() >= 10,
            "NPCs should have spoken, got {}",
            log.turns.len()
        );
        assert!(log.turns.iter().any(|t| t.speaker == "老周"));
        assert!(log.turns.iter().any(|t| t.speaker == "Aria"));
        assert!(log.turns.iter().any(|t| t.speaker == "阿哲"));
        // Memory grew and round-robin partners vary.
        assert!(w.resource::<MemoryStore>().unwrap().len_total() > 5);
        assert!(log.turns.iter().any(|t| t.to != "客人"));
    }

    #[test]
    fn director_advances_story_and_days() {
        let mut w = cafe_world();
        let mut dir = DirectorSystem::new(Arc::new(RuleProvider::new()), 1);
        for _ in 0..6 {
            dir.run(&ctx(), &mut w);
        }
        let s = w.resource::<WorldStory>().unwrap();
        assert_eq!(s.beats.len(), 6);
        assert_eq!(s.day, 3); // every 3rd beat advances the day
    }

    #[test]
    fn cognition_boots_on_a_bare_world_without_panicking() {
        // No DialogueLog / WorldStory / MemoryStore pre-registered —
        // the systems self-heal (resource_or_default) instead of
        // panicking on the first tick. The AI meter appears on its own
        // as soon as a provider call actually measures tokens.
        let mut w = World::new();
        let mut cog = CognitionSystem::new(Arc::new(RuleProvider::new()), 1);
        let mut dir = DirectorSystem::new(Arc::new(RuleProvider::new()), 1);
        for _ in 0..3 {
            cog.run(&ctx(), &mut w);
            dir.run(&ctx(), &mut w);
        }
        assert_eq!(w.resource::<WorldStory>().unwrap().beats.len(), 3);
        let meter = w.resource::<oriera_infer::UsageMeter>().unwrap();
        assert_eq!(meter.calls, 3);
        assert!(meter.total.total() > 0);
    }
}
