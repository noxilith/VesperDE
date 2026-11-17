//! Background blur: the wallpaper (everything below the windows) is rendered once, shrunk, blurred with a few Gaussian
//! passes, and drawn just under each window that asked for it. It is only redone when the wallpaper layers change.

use std::hash::{Hash, Hasher};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            damage::OutputDamageTracker,
            element::{
                texture::{TextureBuffer, TextureRenderElement},
                Element as _, Kind,
            },
            gles::{element::TextureShaderElement, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName, UniformType},
            Bind, Frame, Offscreen, Renderer,
        },
    },
    utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size, Transform},
};

use crate::render::Element;

/// How much smaller the blurred copy is than the screen. Blurring a small image is cheap and looks the same.
const DOWN: i32 = 4;

const GRADE: &str = r#"#version 100
//_DEFINES_
precision mediump float;
uniform sampler2D tex;
uniform float alpha;
uniform vec2 grade;
varying vec2 v_coords;
void main() {
    vec4 c = texture2D(tex, v_coords);
    float luma = dot(c.rgb, vec3(0.299, 0.587, 0.114));
    vec3 rgb = mix(vec3(luma), c.rgb, grade.y) * grade.x;
#if defined(NO_ALPHA)
    c.a = 1.0;
#endif
    gl_FragColor = vec4(clamp(rgb, 0.0, 1.0), c.a) * alpha;
}
"#;

const SHADER: &str = r#"#version 100
//_DEFINES_
precision mediump float;
uniform sampler2D tex;
uniform float alpha;
uniform vec2 step;
varying vec2 v_coords;
void main() {
    vec4 c = texture2D(tex, v_coords) * 0.2270270270;
    c += (texture2D(tex, v_coords + step * 1.3846153846) + texture2D(tex, v_coords - step * 1.3846153846)) * 0.3162162162;
    c += (texture2D(tex, v_coords + step * 3.2307692308) + texture2D(tex, v_coords - step * 3.2307692308)) * 0.0702702703;
#if defined(NO_ALPHA)
    c = vec4(c.rgb, 1.0);
#endif
    gl_FragColor = c * alpha;
}
"#;

#[derive(Default)]
pub struct BlurState {
    program: Option<GlesTexProgram>,
    grade: Option<GlesTexProgram>,
    broken: bool,
    /// One blurred copy of the wallpaper per distinct (strength, brightness, saturation), with the scene signature it was made for.
    cache: Vec<((u32, u32, u32), u64, TextureBuffer<GlesTexture>)>,
    tex_size: (i32, i32),
}

/// Areas that share one blur look.
#[derive(Default, Clone)]
pub struct BlurGroup {
    pub strength: u32,
    /// Percent; 100 = unchanged.
    pub brightness: u32,
    pub saturation: u32,
    /// Corner radius of these areas in logical pixels; negative = use the window corner radius.
    pub radius: i32,
    /// Rectangles relative to the output, in logical pixels.
    pub rects: Vec<Rectangle<i32, Logical>>,
}

/// What to blur on one output this frame.
#[derive(Default, Clone)]
pub struct BlurParams {
    pub groups: Vec<BlurGroup>,
}

fn signature(bg: &[Element], scale: Scale<f64>, size: Size<i32, Physical>) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (size.w, size.h).hash(&mut h);
    for e in bg {
        e.id().hash(&mut h);
        format!("{:?}", e.current_commit()).hash(&mut h);
        let g = e.geometry(scale);
        (g.loc.x, g.loc.y, g.size.w, g.size.h).hash(&mut h);
    }
    h.finish()
}

impl BlurState {
    /// Blurred-background elements for `params.rects`, to be drawn below the windows and above `bg`.
    pub fn elements(
        &mut self,
        renderer: &mut GlesRenderer,
        bg: &[Element],
        size: Size<i32, Physical>,
        scale: Scale<f64>,
        params: &BlurParams,
        style: Option<(&crate::round::RoundShaders, i32)>,
    ) -> Vec<Element> {
        let groups: Vec<&BlurGroup> = params.groups.iter().filter(|g| !g.rects.is_empty()).collect();
        if groups.is_empty() || self.broken || size.w < DOWN * 4 || size.h < DOWN * 4 {
            self.cache.clear();
            return Vec::new();
        }
        let sig = signature(bg, scale, size);
        let keys: Vec<(u32, u32, u32)> = groups.iter().map(|g| (g.strength, g.brightness, g.saturation)).collect();
        self.cache.retain(|(k, _, _)| keys.contains(k));
        self.tex_size = ((size.w + DOWN - 1) / DOWN, (size.h + DOWN - 1) / DOWN);
        let mut out = Vec::new();
        for g in groups {
            let key = (g.strength, g.brightness, g.saturation);
            let fresh = self.cache.iter().any(|(k, s, _)| *k == key && *s == sig);
            if !fresh {
                match self.rebuild(renderer, bg, size, scale.x, key) {
                    Some(buf) => {
                        self.cache.retain(|(k, _, _)| *k != key);
                        self.cache.push((key, sig, buf));
                    }
                    None => {
                        tracing::warn!("background blur is not available on this GPU; switching it off");
                        self.broken = true;
                        return Vec::new();
                    }
                }
            }
            let Some(buf) = self.cache.iter().find(|(k, _, _)| *k == key).map(|(_, _, b)| b) else { continue };
            let style = match (style, g.radius) {
                (Some((sh, r)), gr) => Some((sh, if gr < 0 { r } else { gr })),
                (None, _) => None,
            };
            out.extend(g.rects.iter().map(|r| {
                let loc: Point<f64, Physical> = r.loc.to_f64().to_physical(scale);
                let phys: Size<f64, Physical> = r.size.to_f64().to_physical(scale);
                // the part of the shrunken texture that lies behind the window
                let src = Rectangle::<f64, Logical>::new(
                    (loc.x / DOWN as f64, loc.y / DOWN as f64).into(),
                    (phys.w / DOWN as f64, phys.h / DOWN as f64).into(),
                );
                let el = TextureRenderElement::from_texture_buffer(loc, buf, Some(1.0), Some(src), Some(r.size), Kind::Unspecified);
                match style {
                    Some((shaders, radius)) if radius > 0 => {
                        let ts = self.tex_size;
                        let src_px = Rectangle::<f64, Buffer>::new((src.loc.x, src.loc.y).into(), (src.size.w, src.size.h).into());
                        let u = crate::round::mask_uniforms((ts.0 as f32, ts.1 as f32), src_px, (phys.w as f32, phys.h as f32), (0.0, 0.0, phys.w as f32, phys.h as f32), radius as f32 * scale.x as f32);
                        Element::from(TextureShaderElement::new(el, shaders.tex.clone(), u))
                    }
                    _ => Element::from(el),
                }
            }));
        }
        out
    }

    fn rebuild(
        &mut self,
        renderer: &mut GlesRenderer,
        bg: &[Element],
        size: Size<i32, Physical>,
        scale: f64,
        key: (u32, u32, u32),
    ) -> Option<TextureBuffer<GlesTexture>> {
        let (strength, brightness, saturation) = key;
        if self.program.is_none() {
            self.program = renderer.compile_custom_texture_shader(SHADER, &[UniformName::new("step", UniformType::_2f)]).ok();
        }
        if self.grade.is_none() {
            self.grade = renderer.compile_custom_texture_shader(GRADE, &[UniformName::new("grade", UniformType::_2f)]).ok();
        }
        let grade = self.grade.clone()?;
        let program = self.program.clone()?;

        // 1. the wallpaper at full size
        let full_buf: Size<i32, Buffer> = (size.w, size.h).into();
        let mut full: GlesTexture = Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Abgr8888, full_buf).ok()?;
        {
            let mut target = renderer.bind(&mut full).ok()?;
            let mut tracker = OutputDamageTracker::new(size, scale, Transform::Normal);
            tracker.render_output(renderer, &mut target, 0, bg, [0.08, 0.08, 0.1, 1.0]).ok()?;
        }

        // 2. shrink it
        let small: Size<i32, Physical> = ((size.w + DOWN - 1) / DOWN, (size.h + DOWN - 1) / DOWN).into();
        let small_buf: Size<i32, Buffer> = (small.w, small.h).into();
        let mut a: GlesTexture = Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Abgr8888, small_buf).ok()?;
        let mut b: GlesTexture = Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Abgr8888, small_buf).ok()?;
        let whole = Rectangle::<i32, Physical>::from_size(small);
        copy(renderer, &full, Rectangle::from_size(full_buf.to_f64()), &mut a, small, whole, None, &[])?;

        // 3. blur: horizontal then vertical, a few times; a larger strength = wider steps and more passes
        let strength = strength.clamp(1, 100) as f32;
        let passes = 1 + (strength as u32) / 20;
        let reach = 0.5 + strength * 0.05;
        let src_all: Rectangle<f64, Buffer> = Rectangle::from_size(small_buf.to_f64());
        for _ in 0..passes {
            let h = [Uniform::new("step", (reach / small.w as f32, 0.0f32))];
            copy(renderer, &a, src_all, &mut b, small, whole, Some(&program), &h)?;
            let v = [Uniform::new("step", (0.0f32, reach / small.h as f32))];
            copy(renderer, &b, src_all, &mut a, small, whole, Some(&program), &v)?;
        }
        // Render targets are bottom-up. Flip the picture once here, so the element itself needs no transform (a transformed
        // element drawn through partial damage can show the wrong part of the texture).
        let g = [Uniform::new("grade", (brightness as f32 / 100.0, saturation as f32 / 100.0))];
        copy_with(renderer, &a, src_all, &mut b, small, whole, Some(&grade), &g, Transform::Normal)?;
        Some(TextureBuffer::from_texture(renderer, b, 1, Transform::Normal, None))
    }
}

/// Draw `from` (the `src` part) over the whole of `to`.
#[allow(clippy::too_many_arguments)]
fn copy(
    renderer: &mut GlesRenderer,
    from: &GlesTexture,
    src: Rectangle<f64, Buffer>,
    to: &mut GlesTexture,
    to_size: Size<i32, Physical>,
    dest: Rectangle<i32, Physical>,
    program: Option<&GlesTexProgram>,
    uniforms: &[Uniform<'_>],
) -> Option<()> {
    copy_with(renderer, from, src, to, to_size, dest, program, uniforms, Transform::Normal)
}

#[allow(clippy::too_many_arguments)]
fn copy_with(
    renderer: &mut GlesRenderer,
    from: &GlesTexture,
    src: Rectangle<f64, Buffer>,
    to: &mut GlesTexture,
    to_size: Size<i32, Physical>,
    dest: Rectangle<i32, Physical>,
    program: Option<&GlesTexProgram>,
    uniforms: &[Uniform<'_>],
    transform: Transform,
) -> Option<()> {
    let mut target = renderer.bind(to).ok()?;
    let mut frame = renderer.render(&mut target, to_size, Transform::Normal).ok()?;
    frame.clear([0.0, 0.0, 0.0, 0.0].into(), &[dest]).ok()?;
    frame.render_texture_from_to(from, src, dest, &[dest], &[], transform, 1.0, program, uniforms).ok()?;
    let sync = frame.finish().ok()?;
    let _ = sync.wait();
    Some(())
}
