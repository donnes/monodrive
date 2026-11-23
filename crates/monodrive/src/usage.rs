//! Says this launch happened, once the user has agreed to share anonymous
//! usage, whether they agreed before it or during it.

use crate::preferences::Preferences;
use gpui::App;
use monodrive_core::usage;

pub fn start(cx: &mut App) {
    if !usage::AVAILABLE {
        return;
    }
    let mut sent = false;
    let mut send = move |cx: &mut App| {
        let prefs = cx.global::<Preferences>();
        let Some(id) = prefs
            .usage_id
            .clone()
            .filter(|_| prefs.share_usage && !sent)
        else {
            return;
        };
        sent = true;
        cx.background_executor()
            .spawn(async move {
                let _ = usage::send(&id, "app_opened");
            })
            .detach();
    };
    send(cx);
    cx.observe_global::<Preferences>(send).detach();
}
