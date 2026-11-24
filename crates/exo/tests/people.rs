//! Google's people, end to end on mock-servers: Workspace colleagues in compose's suggestions, and
//! the photos, companies and titles of saved contacts and colleagues in the contact pane.

mod support;

use egui::Key;
use support::{Fixture, start, start_with_contact_photos};

/// Runs frames until `email`'s photo is loaded, which the contact pane asks for.
fn wait_for_photo(app: &mut Fixture, email: &str) -> exo_ui::AvatarPhoto {
    for _ in 0..100 {
        if let Some(photo) = exo::people::photo(&app.harness.ctx, email) {
            return photo;
        }
        app.wait(0.05);
    }
    panic!("no photo of {email}");
}

/// Whether the last frame painted `photo`.
fn paints(app: &Fixture, photo: &exo_ui::AvatarPhoto) -> bool {
    fn paints(shape: &egui::Shape, textures: &[egui::TextureId]) -> bool {
        match shape {
            egui::Shape::Mesh(mesh) => textures.contains(&mesh.texture_id),
            egui::Shape::Vec(shapes) => shapes.iter().any(|shape| paints(shape, textures)),
            _ => false,
        }
    }
    let textures: Vec<egui::TextureId> = photo.textures().collect();
    app.harness
        .output()
        .shapes
        .iter()
        .any(|clipped| paints(&clipped.shape, &textures))
}

#[test]
fn a_colleague_only_the_directory_knows_is_suggested() {
    let mut app = start("people-directory");
    app.key(Key::C);
    app.wait(0.5);
    app.type_text("haddad");
    app.wait(0.5);
    app.key(Key::Enter);
    assert_eq!(
        app.mail().compose_focus().map(|(to, _)| to),
        Some(vec!["noor@brightmoor.co".to_owned()]),
        "Noor, whom no mail names"
    );
}

#[test]
fn the_contact_pane_shows_a_colleagues_photo_company_and_title() {
    let mut app = start_with_contact_photos("people-pane");
    assert_eq!(app.mail().contact_pane_email(), Some("tom@brightmoor.co"));
    let tom = exo::people::person(&app.harness.ctx, "tom@brightmoor.co").expect("listed");
    assert_eq!(
        (tom.company.as_deref(), tom.title.as_deref()),
        (Some("Brightmoor"), Some("Product Designer"))
    );
    let photo = wait_for_photo(&mut app, "tom@brightmoor.co");
    app.wait(0.1);
    assert!(paints(&app, &photo), "the pane draws it");
}

#[test]
fn only_googles_own_photos_show() {
    let app = start("people-default-photo");
    let tom = exo::people::person(&app.harness.ctx, "tom@brightmoor.co").expect("listed");
    assert_eq!(tom.photo, None, "Google's generated avatar isn't his");
    assert_eq!(tom.title.as_deref(), Some("Product Designer"));
    assert!(
        exo::people::person(&app.harness.ctx, "yusuf@tinkerform.io").is_none(),
        "an other contact brings nothing but a name"
    );
}
