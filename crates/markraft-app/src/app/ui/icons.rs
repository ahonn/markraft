use gpui::{Bounds, Hsla, IntoElement, PathBuilder, Styled, canvas, fill, point, px, rgb, size};

#[derive(Clone, Copy)]
pub(super) enum Icon {
    Plus,
    Notes,
    Command,
    Text,
    Close,
    ChevronDown,
    Check,
    Pin,
    Trash,
    Copy,
    Export,
    Settings,
    Bold,
    Italic,
    Code,
    Strikethrough,
    Underline,
    Link,
    Edit,
    Open,
    Heading,
    Quote,
    CodeBlock,
    Paragraph,
    Ordered,
    Bullet,
    Task,
    Divider,
    Restore,
}

pub(super) fn icon(kind: Icon, color: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            let mut path = PathBuilder::stroke(px(1.3));
            match kind {
                Icon::Plus => {
                    line(&mut path, &[(8., 3.), (8., 13.)]);
                    line(&mut path, &[(3., 8.), (13., 8.)]);
                }
                Icon::Notes => {
                    line(
                        &mut path,
                        &[(4., 4.), (2., 4.), (2., 14.), (10., 14.), (10., 12.)],
                    );
                    rounded_rect(&mut path, 5., 1.5, 8., 10., 1.5);
                }
                Icon::Command => {
                    rounded_rect(&mut path, 2., 2., 4., 4., 2.);
                    rounded_rect(&mut path, 10., 2., 4., 4., 2.);
                    rounded_rect(&mut path, 2., 10., 4., 4., 2.);
                    rounded_rect(&mut path, 10., 10., 4., 4., 2.);
                    line(&mut path, &[(6., 2.), (6., 14.)]);
                    line(&mut path, &[(10., 2.), (10., 14.)]);
                    line(&mut path, &[(2., 6.), (14., 6.)]);
                    line(&mut path, &[(2., 10.), (14., 10.)]);
                }
                Icon::Text => {
                    line(&mut path, &[(3., 4.), (3., 2.5), (13., 2.5), (13., 4.)]);
                    line(&mut path, &[(8., 2.5), (8., 13.5)]);
                    line(&mut path, &[(5.5, 13.5), (10.5, 13.5)]);
                }
                Icon::Close => {
                    window.paint_quad(
                        fill(
                            Bounds::new(
                                bounds.origin + point(px(2.), px(2.)),
                                size(px(12.), px(12.)),
                            ),
                            color,
                        )
                        .corner_radii(px(6.)),
                    );
                    line(&mut path, &[(6., 6.), (10., 10.)]);
                    line(&mut path, &[(10., 6.), (6., 10.)]);
                }
                Icon::ChevronDown => line(&mut path, &[(5., 6.5), (8., 9.5), (11., 6.5)]),
                Icon::Check => line(&mut path, &[(3., 8.), (6.5, 11.5), (13., 4.5)]),
                Icon::Pin => {
                    line(
                        &mut path,
                        &[
                            (5., 2.5),
                            (11., 2.5),
                            (10.5, 7.),
                            (12.5, 9.5),
                            (3.5, 9.5),
                            (5.5, 7.),
                            (5., 2.5),
                        ],
                    );
                    line(&mut path, &[(8., 9.5), (8., 14.)]);
                }
                Icon::Trash => {
                    line(&mut path, &[(2.5, 4.5), (13.5, 4.5)]);
                    line(
                        &mut path,
                        &[(5.5, 4.5), (5.5, 2.5), (10.5, 2.5), (10.5, 4.5)],
                    );
                    line(
                        &mut path,
                        &[(4., 4.5), (4.5, 13.5), (11.5, 13.5), (12., 4.5)],
                    );
                    line(&mut path, &[(6.5, 7.), (6.5, 11.)]);
                    line(&mut path, &[(9.5, 7.), (9.5, 11.)]);
                }
                Icon::Copy => {
                    rounded_rect(&mut path, 5.5, 5.5, 8., 8., 1.5);
                    line(
                        &mut path,
                        &[
                            (10.5, 3.5),
                            (10.5, 2.5),
                            (2.5, 2.5),
                            (2.5, 10.5),
                            (3.5, 10.5),
                        ],
                    );
                }
                Icon::Export => {
                    line(&mut path, &[(3., 8.5), (3., 13.5), (13., 13.5), (13., 8.5)]);
                    line(&mut path, &[(8., 10.), (8., 2.)]);
                    line(&mut path, &[(4.5, 5.5), (8., 2.), (11.5, 5.5)]);
                }
                Icon::Settings => {
                    for (y, knob) in [(4., 6.), (8., 10.), (12., 5.)] {
                        line(&mut path, &[(2., y), (knob - 1.5, y)]);
                        line(&mut path, &[(knob + 1.5, y), (14., y)]);
                        circle(&mut path, knob, y, 1.5);
                    }
                }
                Icon::Bold => {
                    path.move_to(point(px(5.), px(2.5)));
                    path.line_to(point(px(8.5), px(2.5)));
                    path.cubic_bezier_to(
                        point(px(8.5), px(7.5)),
                        point(px(12.5), px(2.5)),
                        point(px(12.5), px(7.5)),
                    );
                    path.line_to(point(px(5.), px(7.5)));
                    path.move_to(point(px(8.5), px(7.5)));
                    path.cubic_bezier_to(
                        point(px(8.5), px(13.5)),
                        point(px(13.), px(7.5)),
                        point(px(13.), px(13.5)),
                    );
                    path.line_to(point(px(5.), px(13.5)));
                    path.line_to(point(px(5.), px(2.5)));
                }
                Icon::Italic => {
                    line(&mut path, &[(7., 2.5), (12., 2.5)]);
                    line(&mut path, &[(9.5, 2.5), (6.5, 13.5)]);
                    line(&mut path, &[(4., 13.5), (9., 13.5)]);
                }
                Icon::Strikethrough => {
                    line(&mut path, &[(11.5, 4.), (9.5, 2.5), (6., 2.5), (4.5, 4.5)]);
                    line(&mut path, &[(4.5, 4.5), (5., 6.5)]);
                    line(&mut path, &[(11., 9.5), (11.5, 11.5), (10., 13.5)]);
                    line(&mut path, &[(10., 13.5), (6., 13.5), (4.5, 12.)]);
                    line(&mut path, &[(2., 8.), (14., 8.)]);
                }
                Icon::Underline => {
                    line(
                        &mut path,
                        &[(4.5, 2.5), (4.5, 8.), (6., 10.5), (10., 10.5), (11.5, 8.)],
                    );
                    line(&mut path, &[(11.5, 8.), (11.5, 2.5)]);
                    line(&mut path, &[(3.5, 13.5), (12.5, 13.5)]);
                }
                Icon::Edit => {
                    line(
                        &mut path,
                        &[
                            (3., 13.),
                            (3.5, 10.5),
                            (10.5, 3.5),
                            (12.5, 5.5),
                            (5.5, 12.5),
                            (3., 13.),
                        ],
                    );
                    line(&mut path, &[(9., 5.), (11., 7.)]);
                }
                Icon::Open => {
                    line(&mut path, &[(4., 12.), (12., 4.)]);
                    line(&mut path, &[(6., 4.), (12., 4.), (12., 10.)]);
                }
                Icon::Link => {
                    line(
                        &mut path,
                        &[(7., 5.), (9., 3.), (11.5, 3.), (13., 4.5), (13., 7.)],
                    );
                    line(&mut path, &[(13., 7.), (11., 9.)]);
                    line(
                        &mut path,
                        &[(9., 11.), (7., 13.), (4.5, 13.), (3., 11.5), (3., 9.)],
                    );
                    line(&mut path, &[(3., 9.), (5., 7.)]);
                    line(&mut path, &[(6., 10.), (10., 6.)]);
                }
                Icon::Code => {
                    line(&mut path, &[(5., 4.), (1.5, 8.), (5., 12.)]);
                    line(&mut path, &[(11., 4.), (14.5, 8.), (11., 12.)]);
                    line(&mut path, &[(9., 2.5), (7., 13.5)]);
                }
                Icon::Heading => {
                    line(&mut path, &[(3.5, 2.5), (3.5, 13.5)]);
                    line(&mut path, &[(12.5, 2.5), (12.5, 13.5)]);
                    line(&mut path, &[(3.5, 8.), (12.5, 8.)]);
                }
                Icon::Quote => {
                    line(&mut path, &[(3., 3.), (3., 13.)]);
                    line(&mut path, &[(6.5, 5.), (13., 5.)]);
                    line(&mut path, &[(6.5, 8.), (13., 8.)]);
                    line(&mut path, &[(6.5, 11.), (10.5, 11.)]);
                }
                Icon::Ordered => {
                    line(&mut path, &[(2., 3.5), (3.5, 2.5), (3.5, 6.5)]);
                    line(&mut path, &[(2., 10.), (4.5, 10.), (2., 13.5), (4.5, 13.5)]);
                    line(&mut path, &[(7.5, 4.5), (14., 4.5)]);
                    line(&mut path, &[(7.5, 11.5), (14., 11.5)]);
                }
                Icon::CodeBlock => {
                    rounded_rect(&mut path, 1.5, 2.5, 13., 11., 2.);
                    line(&mut path, &[(6., 5.5), (3.8, 8.), (6., 10.5)]);
                    line(&mut path, &[(10., 5.5), (12.2, 8.), (10., 10.5)]);
                }
                Icon::Divider => line(&mut path, &[(2., 8.), (14., 8.)]),
                Icon::Paragraph => {
                    path.move_to(point(px(9.), px(8.5)));
                    path.line_to(point(px(6.5), px(8.5)));
                    path.cubic_bezier_to(
                        point(px(6.5), px(2.5)),
                        point(px(2.), px(8.5)),
                        point(px(2.), px(2.5)),
                    );
                    path.line_to(point(px(13.), px(2.5)));
                    line(&mut path, &[(9., 2.5), (9., 13.5)]);
                    line(&mut path, &[(12., 2.5), (12., 13.5)]);
                }
                Icon::Bullet => {
                    for y in [4., 8., 12.] {
                        circle(&mut path, 3., y, 0.65);
                        line(&mut path, &[(6., y), (13.5, y)]);
                    }
                }
                Icon::Task => {
                    rounded_rect(&mut path, 2.5, 2.5, 11., 11., 2.);
                    line(&mut path, &[(5., 8.), (7., 10.), (11., 6.)]);
                }
                Icon::Restore => {
                    path.move_to(point(px(3.), px(6.5)));
                    path.cubic_bezier_to(
                        point(px(13.5), px(8.)),
                        point(px(5.), px(0.5)),
                        point(px(13.5), px(2.5)),
                    );
                    path.cubic_bezier_to(
                        point(px(4.), px(12.)),
                        point(px(13.5), px(13.5)),
                        point(px(7.), px(15.5)),
                    );
                    line(&mut path, &[(2.5, 2.5), (2.5, 6.5), (6.5, 6.5)]);
                }
            }
            path.translate(bounds.origin);
            if let Ok(path) = path.build() {
                window.paint_path(
                    path,
                    if matches!(kind, Icon::Close) {
                        if color.l < 0.5 {
                            rgb(0xffffff).into()
                        } else {
                            rgb(0x2e2f33).into()
                        }
                    } else {
                        color
                    },
                );
            }
        },
    )
    .size(px(16.))
    .flex_shrink_0()
}

fn line(path: &mut PathBuilder, points: &[(f32, f32)]) {
    if let Some((first, rest)) = points.split_first() {
        path.move_to(point(px(first.0), px(first.1)));
        for &(x, y) in rest {
            path.line_to(point(px(x), px(y)));
        }
    }
}

fn circle(path: &mut PathBuilder, x: f32, y: f32, radius: f32) {
    let tangent = radius * 0.552_284_8;
    path.move_to(point(px(x + radius), px(y)));
    path.cubic_bezier_to(
        point(px(x), px(y + radius)),
        point(px(x + radius), px(y + tangent)),
        point(px(x + tangent), px(y + radius)),
    );
    path.cubic_bezier_to(
        point(px(x - radius), px(y)),
        point(px(x - tangent), px(y + radius)),
        point(px(x - radius), px(y + tangent)),
    );
    path.cubic_bezier_to(
        point(px(x), px(y - radius)),
        point(px(x - radius), px(y - tangent)),
        point(px(x - tangent), px(y - radius)),
    );
    path.cubic_bezier_to(
        point(px(x + radius), px(y)),
        point(px(x + tangent), px(y - radius)),
        point(px(x + radius), px(y - tangent)),
    );
    path.close();
}

fn rounded_rect(path: &mut PathBuilder, x: f32, y: f32, width: f32, height: f32, radius: f32) {
    path.move_to(point(px(x + radius), px(y)));
    path.line_to(point(px(x + width - radius), px(y)));
    path.curve_to(
        point(px(x + width), px(y + radius)),
        point(px(x + width), px(y)),
    );
    path.line_to(point(px(x + width), px(y + height - radius)));
    path.curve_to(
        point(px(x + width - radius), px(y + height)),
        point(px(x + width), px(y + height)),
    );
    path.line_to(point(px(x + radius), px(y + height)));
    path.curve_to(
        point(px(x), px(y + height - radius)),
        point(px(x), px(y + height)),
    );
    path.line_to(point(px(x), px(y + radius)));
    path.curve_to(point(px(x + radius), px(y)), point(px(x), px(y)));
    path.close();
}
