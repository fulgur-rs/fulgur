//! box-shadow rendering tests.

use fulgur::engine::Engine;

fn make_engine() -> Engine {
    Engine::builder().build()
}

#[test]
fn renders_basic_offset_shadow_without_error() {
    let html = r#"
    <!DOCTYPE html><html><body>
      <div style="width:100px;height:100px;background:#eee;
                  box-shadow: 4px 4px 0 #888;">hi</div>
    </body></html>"#;
    let pdf = make_engine().render_html(html).expect("render ok");
    assert!(!pdf.is_empty());
}

#[test]
fn renders_multiple_shadows() {
    let html = r#"
    <!DOCTYPE html><html><body>
      <div style="width:100px;height:100px;
                  box-shadow: 2px 2px 0 red, 4px 4px 0 blue;"></div>
    </body></html>"#;
    let pdf = make_engine().render_html(html).expect("render ok");
    assert!(!pdf.is_empty());
}

#[test]
fn renders_shadow_with_rgba_alpha() {
    let html = r#"
    <!DOCTYPE html><html><body>
      <div style="width:100px;height:100px;
                  box-shadow: 2px 2px 0 rgba(0,0,0,0.5);"></div>
    </body></html>"#;
    let pdf = make_engine().render_html(html).expect("render ok");
    assert!(!pdf.is_empty());
}

#[test]
fn renders_shadow_with_spread() {
    let html = r#"
    <!DOCTYPE html><html><body>
      <div style="width:100px;height:100px;
                  box-shadow: 0 0 0 4px red;"></div>
    </body></html>"#;
    let pdf = make_engine().render_html(html).expect("render ok");
    assert!(!pdf.is_empty());
}

#[test]
fn renders_shadow_with_negative_spread() {
    let html = r#"
    <!DOCTYPE html><html><body>
      <div style="width:100px;height:100px;
                  box-shadow: 0 0 0 -2px red;"></div>
    </body></html>"#;
    let pdf = make_engine().render_html(html).expect("render ok");
    assert!(!pdf.is_empty());
}

#[test]
fn renders_shadow_with_border_radius() {
    let html = r#"
    <!DOCTYPE html><html><body>
      <div style="width:100px;height:100px;border-radius:20px;
                  box-shadow: 4px 4px 0 2px #888;"></div>
    </body></html>"#;
    let pdf = make_engine().render_html(html).expect("render ok");
    assert!(!pdf.is_empty());
}
