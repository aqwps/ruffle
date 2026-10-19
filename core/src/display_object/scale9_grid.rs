//! Nine-slice scaling of vector artwork. Child clips and text keep their ordinary transforms.

use super::{BoundsMode, DisplayObject, TDisplayObject};
use crate::context::RenderContext;
use ruffle_render::backend::{RenderBackend, ShapeHandle};
use ruffle_render::bitmap::BitmapSource;
use ruffle_render::commands::CommandHandler;
use ruffle_render::matrix::Matrix;
use ruffle_render::shape_utils::{DistilledShape, DrawPath};
use ruffle_render::transform::Transform;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use swf::{Color, FillStyle, Rectangle, Twips};

/// Gradient coordinates use the ordinary transform, even when the outline is
/// sliced. Keep a bounded cache of shape handles with compensated fill matrices.
#[derive(Clone, Debug, Default)]
pub(crate) struct GradientCache {
    has_gradients: Cell<Option<bool>>,
    handles: RefCell<VecDeque<(Matrix, f32, ShapeHandle)>>,
}

impl GradientCache {
    pub(crate) fn clear(&mut self) {
        self.has_gradients.set(None);
        self.handles.get_mut().clear();
    }

    pub(crate) fn get<'a>(
        &self,
        renderer: &mut dyn RenderBackend,
        source: &dyn BitmapSource,
        base: ShapeHandle,
        correction: Matrix,
        scale: f32,
        shape: impl FnOnce() -> DistilledShape<'a>,
    ) -> ShapeHandle {
        if correction == Matrix::IDENTITY || self.has_gradients.get() == Some(false) {
            return base;
        }
        let mut handles = self.handles.borrow_mut();
        if let Some(index) = handles
            .iter()
            .position(|(m, s, _)| *m == correction && *s == scale)
        {
            let entry = handles.remove(index).unwrap();
            let handle = entry.2.clone();
            handles.push_back(entry);
            return handle;
        }
        let shape = shape();
        let has_gradients = shape.paths.iter().any(|path| {
            let style = match path {
                DrawPath::Fill { style, .. } => *style,
                DrawPath::Stroke { style, .. } => style.fill_style(),
            };
            matches!(
                style,
                FillStyle::LinearGradient(_)
                    | FillStyle::RadialGradient(_)
                    | FillStyle::FocalGradient { .. }
            )
        });
        self.has_gradients.set(Some(has_gradients));
        let Some(inverse) = correction.inverse().filter(|_| has_gradients) else {
            return base;
        };
        let compensate = |style: &FillStyle| {
            let mut style = style.clone();
            match &mut style {
                FillStyle::LinearGradient(g)
                | FillStyle::RadialGradient(g)
                | FillStyle::FocalGradient { gradient: g, .. } => {
                    g.matrix = (inverse * Matrix::from(g.matrix)).into();
                }
                _ => {}
            }
            style
        };
        // Keep owned styles alive until the renderer has consumed the paths.
        let fills: Vec<_> = shape
            .paths
            .iter()
            .map(|path| match path {
                DrawPath::Fill { style, .. } => Some(compensate(style)),
                _ => None,
            })
            .collect();
        let lines: Vec<_> = shape
            .paths
            .iter()
            .map(|path| match path {
                DrawPath::Stroke { style, .. } => Some(
                    (*style)
                        .clone()
                        .with_fill_style(compensate(style.fill_style())),
                ),
                _ => None,
            })
            .collect();
        let paths = shape
            .paths
            .into_iter()
            .enumerate()
            .map(|(i, path)| match path {
                DrawPath::Fill {
                    commands,
                    winding_rule,
                    ..
                } => DrawPath::Fill {
                    style: fills[i].as_ref().unwrap(),
                    commands,
                    winding_rule,
                },
                DrawPath::Stroke {
                    commands,
                    is_closed,
                    ..
                } => DrawPath::Stroke {
                    style: lines[i].as_ref().unwrap(),
                    commands,
                    is_closed,
                },
            })
            .collect();
        let handle =
            renderer.register_shape_with_scale(DistilledShape { paths, ..shape }, source, scale);
        if handles.len() == 32 {
            handles.pop_front();
        }
        handles.push_back((correction, scale, handle.clone()));
        handle
    }
}

/// A subdivision in the grid owner's unscaled coordinate system. `transform`
/// corrects the ordinary scale already present on the render transform stack.
#[derive(Debug)]
struct Slice {
    bounds: Rectangle<Twips>,
    transform: Matrix,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct CacheKey {
    bounds: Rectangle<Twips>,
    grid: Rectangle<Twips>,
    matrix: Matrix,
    to_owner: Matrix,
}

pub(super) fn cache_key<'gc>(
    object: DisplayObject<'gc>,
    untransformed_object: Option<DisplayObject<'gc>>,
) -> Option<CacheKey> {
    if DisplayObject::option_ptr_eq(untransformed_object, Some(object)) {
        return None;
    }
    let (owner, to_owner) = if object.scaling_grid().is_valid() {
        (object, Matrix::IDENTITY)
    } else if matches!(
        object,
        DisplayObject::Graphic(_) | DisplayObject::MorphShape(_)
    ) {
        (object.parent()?, object.base().matrix())
    } else {
        return None;
    };
    if DisplayObject::option_ptr_eq(untransformed_object, Some(owner)) {
        return None;
    }
    let grid = owner.scaling_grid();
    grid.is_valid().then(|| CacheKey {
        bounds: owner.bounds(BoundsMode::Engine),
        grid,
        matrix: Matrix {
            tx: Twips::ZERO,
            ty: Twips::ZERO,
            ..owner.base().matrix()
        },
        to_owner,
    })
}

/// Compute the three affine maps along one axis. Scaling happens about the
/// registration point, rather than the top-left of the artwork's bounds.
fn axis(min: Twips, start: Twips, end: Twips, max: Twips, scale: f32) -> Option<[(f32, Twips); 3]> {
    if !scale.is_finite() || scale == 0.0 || min > start || start >= end || end > max {
        return None;
    }

    let size = (max - min).get() as f64;
    let leading = (start - min).get() as f64;
    let trailing = (max - end).get() as f64;
    let scale = f64::from(scale.abs());
    let center = size * scale - leading - trailing;
    // Flash falls back to ordinary scaling once the fixed-size edges no longer fit.
    if center < 1.0 {
        return None;
    }

    let positions = [min, start, end];
    let slopes = [1.0, center / (end - start).get() as f64, 1.0];
    let destinations = [
        min.get() as f64 * scale,
        min.get() as f64 * scale + leading,
        max.get() as f64 * scale - trailing,
    ];
    Some(std::array::from_fn(|i| {
        let slope = slopes[i] / scale;
        let offset = destinations[i] / scale - positions[i].get() as f64 * slope;
        (slope as f32, Twips::new(offset.round() as i32))
    }))
}

fn slices(bounds: Rectangle<Twips>, grid: Rectangle<Twips>, matrix: Matrix) -> Option<Vec<Slice>> {
    if !bounds.is_valid() || !grid.is_valid() || matrix.b != 0.0 || matrix.c != 0.0 {
        return None;
    }
    // Falling below the minimum in one direction need not disable slicing in the other.
    let x = axis(bounds.x_min, grid.x_min, grid.x_max, bounds.x_max, matrix.a)
        .unwrap_or([(1.0, Twips::ZERO); 3]);
    let y = axis(bounds.y_min, grid.y_min, grid.y_max, bounds.y_max, matrix.d)
        .unwrap_or([(1.0, Twips::ZERO); 3]);
    if x.iter()
        .chain(y.iter())
        .all(|&(s, t)| s == 1.0 && t == Twips::ZERO)
    {
        return None;
    }
    // Reject grids outside the artwork rather than cropping away part of the shape.
    if grid.x_min < bounds.x_min
        || grid.x_max > bounds.x_max
        || grid.y_min < bounds.y_min
        || grid.y_max > bounds.y_max
        || grid.width() <= Twips::ZERO
        || grid.height() <= Twips::ZERO
    {
        return None;
    }
    let xs = [bounds.x_min, grid.x_min, grid.x_max, bounds.x_max];
    let ys = [bounds.y_min, grid.y_min, grid.y_max, bounds.y_max];
    let mut result = Vec::with_capacity(9);
    for row in 0..3 {
        for col in 0..3 {
            if xs[col] == xs[col + 1] || ys[row] == ys[row + 1] {
                continue;
            }
            result.push(Slice {
                bounds: Rectangle {
                    x_min: xs[col],
                    x_max: xs[col + 1],
                    y_min: ys[row],
                    y_max: ys[row + 1],
                },
                transform: Matrix {
                    a: x[col].0,
                    d: y[row].0,
                    tx: x[col].1,
                    ty: y[row].1,
                    ..Matrix::IDENTITY
                },
            });
        }
    }
    Some(result)
}

/// Draw only the vector artwork of an object through its own scaling grid, or
/// (for timeline shapes) through its immediate parent's grid. Do not pass child
/// clips/text to this function: Flash scales those normally.
pub(super) fn render<'gc>(
    object: DisplayObject<'gc>,
    context: &mut RenderContext<'_, 'gc>,
    mut draw: impl FnMut(&mut RenderContext<'_, 'gc>, Matrix),
) {
    let Some(key) = cache_key(object, context.untransformed_object) else {
        draw(context, Matrix::IDENTITY);
        return;
    };

    // Nested stencil mask construction cannot express an intersection. Leave
    // mask artwork on the ordinary rendering path until it can be tessellated
    // with the grid instead of clipped with stencil rectangles.
    if context.commands.drawing_mask() {
        draw(context, Matrix::IDENTITY);
        return;
    }
    let Some(from_owner) = key.to_owner.inverse() else {
        draw(context, Matrix::IDENTITY);
        return;
    };
    let Some(slices) = slices(key.bounds, key.grid, key.matrix) else {
        draw(context, Matrix::IDENTITY);
        return;
    };

    let owner_to_screen = context.transform_stack.transform().matrix * from_owner;
    for slice in slices {
        let clip_matrix =
            owner_to_screen * slice.transform * Matrix::create_box_from_rectangle(&slice.bounds);
        context.commands.push_mask();
        context.commands.draw_rect(Color::WHITE, clip_matrix);
        context.commands.activate_mask();
        let correction = from_owner * slice.transform * key.to_owner;
        context.transform_stack.push(&Transform {
            matrix: correction,
            ..Default::default()
        });
        draw(context, correction);
        context.transform_stack.pop();
        context.commands.deactivate_mask();
        context.commands.draw_rect(Color::WHITE, clip_matrix);
        context.commands.pop_mask();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(pixels: i32) -> Twips {
        Twips::from_pixels_i32(pixels)
    }

    #[test]
    fn preserves_edges_and_registration_point() {
        let maps = axis(t(-10), t(10), t(70), t(90), 2.0).unwrap();
        let map = |i: usize, p: i32| {
            Twips::new((t(p).get() as f32 * maps[i].0).round() as i32 + maps[i].1.get()) * 2
        };
        assert_eq!(map(0, -10), t(-20));
        assert_eq!(map(0, 10), t(0));
        assert_eq!(map(1, 10), t(0));
        assert_eq!(map(1, 70), t(160));
        assert_eq!(map(2, 70), t(160));
        assert_eq!(map(2, 90), t(180));
    }

    #[test]
    fn shrinking_and_reflection() {
        let maps = axis(t(0), t(10), t(90), t(100), 0.5).unwrap();
        assert_eq!(maps[0], (2.0, Twips::ZERO));
        assert_eq!(maps[2], (2.0, t(-100)));
        assert_eq!(maps, axis(t(0), t(10), t(90), t(100), -0.5).unwrap());
        assert!(axis(t(0), t(10), t(90), t(100), 0.1).is_none());
    }

    #[test]
    fn rejects_degenerate_axes() {
        for scale in [0.0, f32::INFINITY, f32::NAN] {
            assert!(axis(t(0), t(10), t(90), t(100), scale).is_none());
        }
        assert!(axis(t(0), t(10), t(10), t(100), 2.0).is_none());
        assert!(axis(t(0), t(-1), t(90), t(100), 2.0).is_none());
    }
}
