//! Оформление окон средствами DWM: углы, рамка, подложка, прозрачность (`[decorations]` и правила окон).

use dw_config::{BorderColor, Deco};

use super::*;

#[derive(Default)]
pub(crate) struct DecoState {
    applied: HashMap<WindowId, (Deco, dw_win::DecoSaved)>,
    next: Option<Instant>,
}

fn apply(id: WindowId, want: Deco, saved: &mut dw_win::DecoSaved) {
    dw_win::set_deco(
        id,
        want.corner.map(|c| c as i32),
        want.border.map(|b| match b {
            BorderColor::None => dw_win::BORDER_NONE,
            BorderColor::Rgb(c) => dw_win::colorref(c),
        }),
        want.backdrop.map(|b| b as i32),
        saved,
    );
}

impl App {
    /// Раз в секунду привести оформление окон к настройкам: подхватывает новые окна, правки конфига и снятие правил.
    pub(crate) fn deco_tick(&mut self) {
        let now = Instant::now();
        if self.ext.deco.next.is_some_and(|t| now < t) {
            return;
        }
        self.ext.deco.next = Some(now + Duration::from_secs(1));
        let wants: Vec<(WindowId, Deco)> = self
            .managed
            .iter()
            .filter(|(id, _)| !self.ext.stubs.contains_key(id))
            .map(|(id, m)| {
                (
                    *id,
                    self.resolve(&self.cfg, m).deco.over(self.cfg.decorations),
                )
            })
            .collect();
        for (id, want) in &wants {
            let (prev, mut saved) = self.ext.deco.applied.get(id).copied().unwrap_or_default();
            if prev == *want {
                continue;
            }
            apply(*id, *want, &mut saved);
            if want.opacity != prev.opacity {
                let mut ours = self.ext.opacity.contains(id);
                let _ = dw_win::set_opacity(*id, want.opacity, &mut ours);
                if ours {
                    self.ext.opacity.insert(*id);
                } else {
                    self.ext.opacity.remove(id);
                }
            }
            self.ext.deco.applied.insert(*id, (*want, saved));
        }
        // окна, которых мы больше не ведём, возвращаем как были
        let gone: Vec<WindowId> = self
            .ext
            .deco
            .applied
            .keys()
            .filter(|id| !wants.iter().any(|(w, _)| w == *id))
            .copied()
            .collect();
        for id in gone {
            if let Some((_, mut saved)) = self.ext.deco.applied.remove(&id) {
                apply(id, Deco::default(), &mut saved);
            }
        }
    }

    /// При выходе вернуть углы, рамки и подложки (прозрачность возвращает `shutdown_ext`).
    pub(crate) fn deco_restore_all(&mut self) {
        for (id, (_, mut saved)) in std::mem::take(&mut self.ext.deco.applied) {
            apply(id, Deco::default(), &mut saved);
        }
    }
}
