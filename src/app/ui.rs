//! GPUI Kit setup and handles to each window's independent application view.
use anyhow::{Context as _, Result};
use gpui::*;
use gpui_kit::component::{Root, Theme, ThemeMode};
use std::marker::PhantomData;

struct WindowThemes {
    light: Theme,
    dark: Theme,
}
impl Global for WindowThemes {}

pub fn init(cx: &mut App) {
    if cx.has_global::<WindowThemes>() {
        return;
    }
    gpui_kit::init(cx);
    gpui_kit::component::set_locale("zh-CN");
    Theme::change(ThemeMode::Light, None, cx);
    customize(Theme::global_mut(cx));
    let light = Theme::global(cx).clone();
    Theme::change(ThemeMode::Dark, None, cx);
    customize(Theme::global_mut(cx));
    Theme::global_mut(cx).colors.primary = rgb(0xdaf48a).into();
    Theme::global_mut(cx).colors.primary_foreground = rgb(0x202020).into();
    let dark = Theme::global(cx).clone();
    cx.set_global(light.clone());
    cx.set_global(WindowThemes { light, dark });
}

fn customize(theme: &mut Theme) {
    // The native window and each application surface supply their own backdrop.
    theme.colors.background = rgba(0x00000000).into();
    theme.font_family = super::appearance::font_family().into();
    theme.mono_font_family = super::appearance::mono_font_family().into();
    theme.colors.list_hover = rgba(0x80808020).into();
    theme.colors.list_active = rgba(0x80808030).into();
}

/// Kit 0.5 exposes one global theme. Scope each palette to all descendant
/// rendering phases so settings cannot recolor another window's input text.
pub struct ThemeElement {
    theme: Theme,
    child: AnyElement,
}
pub fn dark(child: impl IntoElement, cx: &mut App) -> ThemeElement {
    themed(child, ThemeMode::Dark, cx)
}
pub fn light(child: impl IntoElement, cx: &mut App) -> ThemeElement {
    themed(child, ThemeMode::Light, cx)
}
fn themed(child: impl IntoElement, mode: ThemeMode, cx: &mut App) -> ThemeElement {
    init(cx);
    let themes = cx.global::<WindowThemes>();
    ThemeElement {
        theme: if mode == ThemeMode::Dark {
            themes.dark.clone()
        } else {
            themes.light.clone()
        },
        child: child.into_any_element(),
    }
}

fn with_theme<R>(theme: &Theme, cx: &mut App, f: impl FnOnce(&mut App) -> R) -> R {
    let original = std::mem::replace(Theme::global_mut(cx), theme.clone());
    let result = f(cx);
    *Theme::global_mut(cx) = original;
    result
}

impl IntoElement for ThemeElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for ThemeElement {
    type RequestLayoutState = ();
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (
            with_theme(&self.theme, cx, |cx| self.child.request_layout(window, cx)),
            (),
        )
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        with_theme(&self.theme, cx, |cx| {
            self.child.prepaint(window, cx);
        });
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        with_theme(&self.theme, cx, |cx| self.child.paint(window, cx));
    }
}

/// The native window is rooted in Kit's Root, with the business view beneath it.
/// No Root, editor, focus, or layout entity is shared between native windows.
pub struct UiWindow<T> {
    handle: AnyWindowHandle,
    marker: PhantomData<fn() -> T>,
}
impl<T> Copy for UiWindow<T> {}
impl<T> Clone for UiWindow<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Render + 'static> UiWindow<T> {
    pub fn from_handle(handle: WindowHandle<Root>) -> Self {
        Self {
            handle: handle.into(),
            marker: PhantomData,
        }
    }

    pub fn from_window(window: &Window) -> Option<Self> {
        if window.root::<Root>().flatten().is_some() || window.root::<T>().flatten().is_some() {
            Some(Self {
                handle: window.window_handle(),
                marker: PhantomData,
            })
        } else {
            None
        }
    }

    fn view(root: AnyView, cx: &App) -> Result<Entity<T>> {
        if let Ok(view) = root.clone().downcast::<T>() {
            return Ok(view);
        }
        root.downcast::<Root>()
            .map_err(|_| anyhow::anyhow!("窗口组件根已改变"))?
            .read(cx)
            .view()
            .clone()
            .downcast::<T>()
            .map_err(|_| anyhow::anyhow!("窗口内容已改变"))
    }

    pub fn update<C: AppContext, R>(
        &self,
        cx: &mut C,
        f: impl FnOnce(&mut T, &mut Window, &mut Context<T>) -> R,
    ) -> Result<R> {
        cx.update_window(self.handle, move |root, window, cx| {
            let view = Self::view(root, cx)?;
            Ok(view.update(cx, |view, cx| f(view, window, cx)))
        })?
    }

    pub fn entity<C: AppContext>(&self, cx: &C) -> Result<Entity<T>> {
        if let Some(handle) = self.handle.downcast::<T>() {
            return handle.entity(cx);
        }
        let handle = self.handle.downcast::<Root>().context("窗口组件根已改变")?;
        cx.read_window(&handle, |root, cx| Self::view(root.into(), cx))?
    }

    pub fn read<'a>(&self, cx: &'a App) -> Result<&'a T> {
        Ok(self.entity(cx)?.read(cx))
    }
}

impl<T> From<UiWindow<T>> for AnyWindowHandle {
    fn from(window: UiWindow<T>) -> Self {
        window.handle
    }
}

pub fn root<T: Render + 'static>(
    view: Entity<T>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<Root> {
    init(cx);
    cx.new(|cx| Root::new(view, window, cx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::component::WindowExt;
    use std::{cell::RefCell, rc::Rc};

    struct WindowFixture {
        value: u32,
    }
    impl Render for WindowFixture {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            dark(
                gpui_kit::component::label::Label::new(self.value.to_string()),
                cx,
            )
        }
    }

    #[::core::prelude::v1::test]
    fn kit_roots_keep_views_independent_and_allow_dialogs_inside_business_updates() {
        let mut app = TestAppContext::single();
        let (first, second) = app.update(|cx| {
            init(cx);
            let mut open = || {
                let handle = cx
                    .open_window(WindowOptions::default(), |window, cx| {
                        let view = cx.new(|_| WindowFixture { value: 0 });
                        root(view, window, cx)
                    })
                    .unwrap();
                UiWindow::<WindowFixture>::from_handle(handle)
            };
            (open(), open())
        });
        first
            .update(&mut app, |view, window, cx| {
                view.value = 7;
                window.open_dialog(cx, |dialog, _, _| dialog.title("组件窗口"));
                assert!(window.has_active_dialog(cx));
                window.close_all_dialogs(cx);
                cx.notify();
            })
            .unwrap();
        app.read(|cx| {
            assert_eq!(first.read(cx).unwrap().value, 7);
            assert_eq!(second.read(cx).unwrap().value, 0);
            assert_ne!(
                first.entity(cx).unwrap().entity_id(),
                second.entity(cx).unwrap().entity_id()
            );
        });
    }

    #[derive(IntoElement)]
    struct ThemeProbe {
        expected: ThemeMode,
        observations: Rc<RefCell<Vec<(&'static str, ThemeMode)>>>,
    }
    impl RenderOnce for ThemeProbe {
        fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
            let expected = self.expected;
            let observations = self.observations;
            let record = move |phase, cx: &App| {
                let theme = Theme::global(cx);
                assert_eq!(theme.mode, expected, "{phase}");
                assert_eq!(theme.colors.background.a, 0.);
                if expected == ThemeMode::Dark {
                    assert!(theme.colors.foreground.l > 0.7);
                } else {
                    assert!(theme.colors.foreground.l < 0.4);
                }
                observations.borrow_mut().push((phase, theme.mode));
            };
            record("layout", cx);
            let prepaint = record.clone();
            canvas(
                move |_, _, cx| prepaint("prepaint", cx),
                move |_, _, _, cx| record("paint", cx),
            )
            .size(px(80.))
        }
    }

    struct MixedThemes {
        observations: Rc<RefCell<Vec<(&'static str, ThemeMode)>>>,
    }
    impl Render for MixedThemes {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let a = dark(
                ThemeProbe {
                    expected: ThemeMode::Dark,
                    observations: self.observations.clone(),
                },
                cx,
            );
            let b = light(
                ThemeProbe {
                    expected: ThemeMode::Light,
                    observations: self.observations.clone(),
                },
                cx,
            );
            div().size_full().child(a).child(b)
        }
    }

    #[::core::prelude::v1::test]
    fn mixed_window_palettes_remain_scoped_through_layout_prepaint_and_paint() {
        let mut app = TestAppContext::single();
        app.update(init);
        let observations = Rc::new(RefCell::new(vec![]));
        let captured = observations.clone();
        let (_, cx) = app.add_window_view(move |_, _| MixedThemes {
            observations: captured,
        });
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        let phases = observations.borrow();
        for phase in ["layout", "prepaint", "paint"] {
            for mode in [ThemeMode::Dark, ThemeMode::Light] {
                assert!(phases.contains(&(phase, mode)), "{phase} {mode:?}");
            }
        }
        cx.read(|cx| assert_eq!(Theme::global(cx).mode, ThemeMode::Light));
    }
}
