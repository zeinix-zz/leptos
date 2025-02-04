use super::{
    MatchInterface, MatchNestedRoutes, PartialPathMatch, PathSegment,
    PossibleRouteMatch, RouteMatchId,
};
use crate::{ChooseView, GeneratedRouteData, MatchParams, Method, SsrMode};
use core::{fmt, iter};
use either_of::Either;
use std::{
    borrow::Cow,
    collections::HashSet,
    sync::atomic::{AtomicU16, Ordering},
};

mod tuples;

pub(crate) static ROUTE_ID: AtomicU16 = AtomicU16::new(1);

#[derive(Debug, PartialEq, Eq)]
pub struct NestedRoute<Segments, Children, Data, View> {
    id: u16,
    segments: Segments,
    children: Option<Children>,
    data: Data,
    view: View,
    methods: HashSet<Method>,
    ssr_mode: SsrMode,
}

impl<Segments, Children, Data, View> Clone
    for NestedRoute<Segments, Children, Data, View>
where
    Segments: Clone,
    Children: Clone,
    Data: Clone,
    View: Clone,
{
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            segments: self.segments.clone(),
            children: self.children.clone(),
            data: self.data.clone(),
            view: self.view.clone(),
            methods: self.methods.clone(),
            ssr_mode: self.ssr_mode.clone(),
        }
    }
}

impl<Segments, View> NestedRoute<Segments, (), (), View> {
    pub fn new(path: Segments, view: View) -> Self
    where
        View: ChooseView,
    {
        Self {
            id: ROUTE_ID.fetch_add(1, Ordering::Relaxed),
            segments: path,
            children: None,
            data: (),
            view,
            methods: [Method::Get].into(),
            ssr_mode: Default::default(),
        }
    }
}

impl<Segments, Data, View> NestedRoute<Segments, (), Data, View> {
    pub fn child<Children>(
        self,
        child: Children,
    ) -> NestedRoute<Segments, Children, Data, View> {
        let Self {
            id,
            segments,
            data,
            view,
            ssr_mode,
            methods,
            ..
        } = self;
        NestedRoute {
            id,
            segments,
            children: Some(child),
            data,
            view,
            ssr_mode,
            methods,
        }
    }

    pub fn ssr_mode(mut self, ssr_mode: SsrMode) -> Self {
        self.ssr_mode = ssr_mode;
        self
    }
}

#[derive(PartialEq, Eq)]
pub struct NestedMatch<Child, View> {
    id: RouteMatchId,
    /// The portion of the full path matched only by this nested route.
    matched: String,
    /// The map of params matched only by this nested route.
    params: Vec<(Cow<'static, str>, String)>,
    /// The nested route.
    child: Option<Child>,
    view_fn: View,
}

impl<Child, View> fmt::Debug for NestedMatch<Child, View>
where
    Child: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NestedMatch")
            .field("matched", &self.matched)
            .field("params", &self.params)
            .field("child", &self.child)
            .finish()
    }
}

impl<Child, View> MatchParams for NestedMatch<Child, View> {
    #[inline(always)]
    fn to_params(&self) -> Vec<(Cow<'static, str>, String)> {
        self.params.clone()
    }
}

impl<Child, View> MatchInterface for NestedMatch<Child, View>
where
    Child: MatchInterface + MatchParams + 'static,
    View: ChooseView,
{
    type Child = Child;

    fn as_id(&self) -> RouteMatchId {
        self.id
    }

    fn as_matched(&self) -> &str {
        &self.matched
    }

    fn into_view_and_child(self) -> (impl ChooseView, Option<Self::Child>) {
        (self.view_fn, self.child)
    }
}

impl<Segments, Children, Data, View> MatchNestedRoutes
    for NestedRoute<Segments, Children, Data, View>
where
    Self: 'static,
    Segments: PossibleRouteMatch + std::fmt::Debug,
    Children: MatchNestedRoutes,
    Children::Match: MatchParams,
    Children: 'static,
    View: ChooseView + Clone,
{
    type Data = Data;
    type Match = NestedMatch<Children::Match, View>;

    fn match_nested<'a>(
        &'a self,
        path: &'a str,
    ) -> (Option<(RouteMatchId, Self::Match)>, &'a str) {
        self.segments
            .test(path)
            .and_then(
                |PartialPathMatch {
                     remaining,
                     mut params,
                     matched,
                 }| {
                    let (_, inner, remaining) = match &self.children {
                        None => (None, None, remaining),
                        Some(children) => {
                            let (inner, remaining) =
                                children.match_nested(remaining);
                            let (id, inner) = inner?;
                            (Some(id), Some(inner), remaining)
                        }
                    };
                    let inner_params = inner
                        .as_ref()
                        .map(|inner| inner.to_params())
                        .unwrap_or_default();

                    let id = RouteMatchId(self.id);

                    if remaining.is_empty() || remaining == "/" {
                        params.extend(inner_params);
                        Some((
                            Some((
                                id,
                                NestedMatch {
                                    id,
                                    matched: matched.to_string(),
                                    params,
                                    child: inner,
                                    view_fn: self.view.clone(),
                                },
                            )),
                            remaining,
                        ))
                    } else {
                        None
                    }
                },
            )
            .unwrap_or((None, path))
    }

    fn generate_routes(
        &self,
    ) -> impl IntoIterator<Item = GeneratedRouteData> + '_ {
        let mut segment_routes = Vec::new();
        self.segments.generate_path(&mut segment_routes);
        let children = self.children.as_ref();
        let ssr_mode = self.ssr_mode.clone();
        let methods = self.methods.clone();
        let regenerate = match &ssr_mode {
            SsrMode::Static(data) => match data.regenerate.as_ref() {
                None => vec![],
                Some(regenerate) => vec![regenerate.clone()],
            },
            _ => vec![],
        };

        match children {
            None => Either::Left(iter::once(GeneratedRouteData {
                segments: segment_routes,
                ssr_mode,
                methods,
                regenerate,
            })),
            Some(children) => {
                Either::Right(children.generate_routes().into_iter().map(
                    move |child| {
                        // extend this route's segments with child segments
                        let segments = segment_routes
                            .clone()
                            .into_iter()
                            .chain(child.segments)
                            .collect();

                        let mut methods = methods.clone();
                        methods.extend(child.methods);

                        let mut regenerate = regenerate.clone();
                        regenerate.extend(child.regenerate);

                        if child.ssr_mode > ssr_mode {
                            GeneratedRouteData {
                                segments,
                                ssr_mode: child.ssr_mode,
                                methods,
                                regenerate,
                            }
                        } else {
                            GeneratedRouteData {
                                segments,
                                ssr_mode: ssr_mode.clone(),
                                methods,
                                regenerate,
                            }
                        }
                    },
                ))
            }
        }
    }
}
