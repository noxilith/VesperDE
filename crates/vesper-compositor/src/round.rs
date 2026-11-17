//! Rounded window corners: a texture shader that masks a window's surface, the same mask for the blurred background
//! behind it, and a pixel shader that draws a rounded border outline.

use smithay::{
    backend::renderer::{
        element::{
            surface::{WaylandSurfaceRenderElement, WaylandSurfaceTexture},
            Element, Id, Kind, RenderElement, UnderlyingStorage,
        },
        gles::{GlesError, GlesFrame, GlesPixelProgram, GlesRenderer, GlesTexProgram, Uniform, UniformName, UniformType},
        utils::{CommitCounter, DamageSet, OpaqueRegions},
        Texture,
    },
    utils::{Buffer, Physical, Point, Rectangle, Scale, Transform},
};

const MASK_TEX: &str = r#"#version 100
//_DEFINES_
precision highp float;
uniform sampler2D tex;
uniform float alpha;
uniform vec2 tex_size;
uniform vec4 src_px;
uniform vec2 dst_size;
uniform vec4 geo;
uniform float radius;
varying vec2 v_coords;
float sdf(vec2 p, vec2 half_size, float r) {
    vec2 q = abs(p) - half_size + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}
void main() {
    vec4 c = texture2D(tex, v_coords);
#if defined(NO_ALPHA)
    c = vec4(c.rgb, 1.0);
#endif
    vec2 pos = ((v_coords * tex_size - src_px.xy) / src_px.zw) * dst_size;
    float d = sdf(pos - geo.xy - geo.zw * 0.5, geo.zw * 0.5, radius);
    gl_FragColor = c * alpha * clamp(0.5 - d, 0.0, 1.0);
}
"#;

const BORDER: &str = r#"
precision highp float;
uniform float alpha;
uniform vec2 size;
uniform float radius;
uniform float bw;
uniform vec4 color;
varying vec2 v_coords;
float sdf(vec2 p, vec2 half_size, float r) {
    vec2 q = abs(p) - half_size + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}
void main() {
    vec2 p = v_coords * size - size * 0.5;
    float outer_d = sdf(p, size * 0.5, radius + bw);
    float inner_d = sdf(p, size * 0.5 - vec2(bw), radius);
    float a = clamp(0.5 - outer_d, 0.0, 1.0) * clamp(0.5 + inner_d, 0.0, 1.0);
    gl_FragColor = vec4(color.rgb * color.a, color.a) * (a * alpha);
}
"#;

const HINT: &str = r#"
precision highp float;
uniform float alpha;
uniform vec2 size;
uniform float radius;
uniform float bw;
uniform vec4 fill;
uniform vec4 edge;
varying vec2 v_coords;
float sdf(vec2 p, vec2 half_size, float r) {
    vec2 q = abs(p) - half_size + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}
void main() {
    vec2 p = v_coords * size - size * 0.5;
    float inside = clamp(0.5 - sdf(p, size * 0.5, radius), 0.0, 1.0);
    float core = clamp(0.5 + sdf(p, size * 0.5 - vec2(bw), max(radius - bw, 0.0)), 0.0, 1.0);
    float ring = inside * (1.0 - core);
    vec4 f = vec4(fill.rgb * fill.a, fill.a) * (inside * core);
    vec4 e = vec4(edge.rgb * edge.a, edge.a) * ring;
    gl_FragColor = (f + e) * alpha;
}
"#;

#[derive(Clone)]
pub struct RoundShaders {
    pub tex: GlesTexProgram,
    pub border: GlesPixelProgram,
    pub hint: GlesPixelProgram,
}

impl RoundShaders {
    pub fn compile(renderer: &mut GlesRenderer) -> Option<Self> {
        let tex = renderer
            .compile_custom_texture_shader(
                MASK_TEX,
                &[
                    UniformName::new("tex_size", UniformType::_2f),
                    UniformName::new("src_px", UniformType::_4f),
                    UniformName::new("dst_size", UniformType::_2f),
                    UniformName::new("geo", UniformType::_4f),
                    UniformName::new("radius", UniformType::_1f),
                ],
            )
            .map_err(|e| tracing::warn!("rounded corners unavailable: {e}"))
            .ok()?;
        let border = renderer
            .compile_custom_pixel_shader(
                BORDER,
                &[UniformName::new("radius", UniformType::_1f), UniformName::new("bw", UniformType::_1f), UniformName::new("color", UniformType::_4f)],
            )
            .map_err(|e| tracing::warn!("rounded borders unavailable: {e}"))
            .ok()?;
        let hint = renderer
            .compile_custom_pixel_shader(
                HINT,
                &[
                    UniformName::new("radius", UniformType::_1f),
                    UniformName::new("bw", UniformType::_1f),
                    UniformName::new("fill", UniformType::_4f),
                    UniformName::new("edge", UniformType::_4f),
                ],
            )
            .map_err(|e| tracing::warn!("drop hints unavailable: {e}"))
            .ok()?;
        Some(Self { tex, border, hint })
    }
}

/// Uniforms for the mask shader.
pub fn mask_uniforms(tex_size: (f32, f32), src: Rectangle<f64, Buffer>, dst_size: (f32, f32), geo: (f32, f32, f32, f32), radius: f32) -> Vec<Uniform<'static>> {
    vec![
        Uniform::new("tex_size", tex_size),
        Uniform::new("src_px", (src.loc.x as f32, src.loc.y as f32, src.size.w as f32, src.size.h as f32)),
        Uniform::new("dst_size", dst_size),
        Uniform::new("geo", geo),
        Uniform::new("radius", radius),
    ]
}

/// A window surface drawn through the rounded-corner mask.
pub struct RoundedSurface {
    inner: WaylandSurfaceRenderElement<GlesRenderer>,
    program: GlesTexProgram,
    /// Corner radius in physical pixels.
    radius: f32,
    /// The window proper (without any shadow the app draws around it), relative to this element, physical pixels.
    geo: (f32, f32, f32, f32),
}

impl RoundedSurface {
    pub fn new(inner: WaylandSurfaceRenderElement<GlesRenderer>, program: GlesTexProgram, radius: f32, geo: (f32, f32, f32, f32)) -> Self {
        Self { inner, program, radius, geo }
    }
}

impl Element for RoundedSurface {
    fn id(&self) -> &Id {
        self.inner.id()
    }
    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }
    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }
    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }
    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        self.inner.location(scale)
    }
    fn transform(&self) -> Transform {
        self.inner.transform()
    }
    fn damage_since(&self, scale: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }
    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        // the corners are see-through, so only the part away from them counts as opaque (the rest of the window lets the
        // renderer skip drawing what is hidden below it)
        let r = self.radius.ceil() as i32 + 1;
        self.inner
            .opaque_regions(scale)
            .into_iter()
            .filter_map(|reg| {
                let size = (reg.size.w - 2 * r, reg.size.h - 2 * r);
                (size.0 > 0 && size.1 > 0).then(|| Rectangle::new((reg.loc.x + r, reg.loc.y + r).into(), size.into()))
            })
            .collect()
    }
    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }
    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl RenderElement<GlesRenderer> for RoundedSurface {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        match self.inner.texture() {
            WaylandSurfaceTexture::Texture(t) => {
                let ts = t.size();
                let uniforms = mask_uniforms((ts.w as f32, ts.h as f32), src, (dst.size.w as f32, dst.size.h as f32), self.geo, self.radius);
                frame.render_texture_from_to(t, src, dst, damage, &[], self.inner.transform(), self.inner.alpha(), Some(&self.program), &uniforms)
            }
            _ => RenderElement::<GlesRenderer>::draw(&self.inner, frame, src, dst, damage, opaque_regions),
        }
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}
