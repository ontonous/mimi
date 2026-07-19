use crate::ast::Type;
#[cfg(test)]
use crate::core::resolved::BackendProfile;
use crate::core::unification::ResolveError;

/// A type as written by the user in source code.
#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceTy(Type);

impl SurfaceTy {
    pub fn new(ty: Type) -> Result<Self, ResolveError> {
        if contains_inference_variables(&ty) {
            Err(ResolveError::DepthOverflow(
                "surface type contains checker-only TypeVar/ForAll".into(),
            ))
        } else {
            Ok(Self(ty))
        }
    }

    pub fn as_type(&self) -> &Type {
        &self.0
    }

    pub fn into_infer(self) -> InferTy {
        InferTy(self.0)
    }
}

/// A type during the inference phase.
#[derive(Debug, Clone, PartialEq)]
pub struct InferTy(Type);

impl InferTy {
    pub(crate) fn from_type(ty: Type) -> Self {
        Self(ty)
    }

    pub fn as_type(&self) -> &Type {
        &self.0
    }

    pub fn into_surface(self) -> Result<SurfaceTy, String> {
        if contains_infer_artifacts(&self.0) {
            Err("type still contains inference artifacts (TypeVar/ForAll)".into())
        } else {
            Ok(SurfaceTy(self.0))
        }
    }
}

/// A fully resolved type with all inference variables resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct ZonkedTy(Type);

impl ZonkedTy {
    pub fn from_resolved(ty: Type) -> Result<Self, ResolveError> {
        crate::core::unification::scan_residual(&ty)?;
        Ok(Self(ty))
    }

    pub fn from_infer(
        ty: &InferTy,
        table: &mut crate::core::unification::UnificationTable,
    ) -> Result<Self, ResolveError> {
        table.zonk(&ty.0).map(ZonkedTy)
    }

    pub fn as_type(&self) -> &Type {
        &self.0
    }

    /// Finalize an executable value type. Unbound inference variables at this
    /// boundary denote payloads which cannot be materialized by the expression
    /// (`None`, the error side of `Ok`, or an empty collection), so their
    /// canonical residual is `Nothing`. Escape placeholders remain errors.
    pub(crate) fn from_expression_type(
        ty: &Type,
        table: &mut crate::core::unification::UnificationTable,
    ) -> Result<Self, ResolveError> {
        struct CompleteUninhabited;

        impl crate::core::type_folder::TypeFolder for CompleteUninhabited {
            fn fold_leaf(&mut self, ty: Type) -> Type {
                match ty {
                    Type::TypeVar(_) => Type::Nothing,
                    ty => ty,
                }
            }
        }

        let resolved = table.resolve_infer(ty)?;
        let completed = crate::core::type_folder::walk_type(resolved, &mut CompleteUninhabited);
        Self::from_resolved(completed)
    }
}

impl From<ZonkedTy> for Type {
    fn from(ty: ZonkedTy) -> Self {
        ty.0
    }
}

/// A type ready for backend consumption.
#[derive(Debug, Clone, PartialEq)]
pub struct BackendTy(Type);

impl BackendTy {
    pub fn from_zonked(
        ty: ZonkedTy,
        profile: &crate::core::resolved::BackendProfile,
    ) -> Result<Self, BackendTypeError> {
        if contains_dynamic_type(ty.as_type())
            && *profile != crate::core::resolved::BackendProfile::Interpreter
        {
            return Err(BackendTypeError::UnsupportedDynamicValue(*profile));
        }
        Ok(Self(ty.0))
    }

    pub fn as_type(&self) -> &Type {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendTypeError {
    UnsupportedDynamicValue(crate::core::resolved::BackendProfile),
}

impl std::fmt::Display for BackendTypeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedDynamicValue(profile) => write!(
                formatter,
                "backend {profile:?} does not support type.dynamic_value"
            ),
        }
    }
}

impl std::error::Error for BackendTypeError {}

/// Stable identity for an inference variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InferVarId(pub u32);

/// A polymorphic type stored separately from monotypes.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeScheme {
    binders: Vec<InferVarId>,
    body: Type,
}

impl TypeScheme {
    pub fn new(binders: Vec<InferVarId>, body: Type) -> Result<Self, ResolveError> {
        let allowed: std::collections::HashSet<u32> =
            binders.iter().map(|binder| binder.0).collect();
        let mut collector = crate::core::type_folder::CollectVarsFolder::new();
        crate::core::type_folder::walk_type(body.clone(), &mut collector);
        if let Some(unbound) = collector
            .vars
            .into_iter()
            .find(|variable| !allowed.contains(variable))
        {
            return Err(ResolveError::UnboundVar(unbound));
        }
        Ok(Self { binders, body })
    }

    pub fn mono(body: Type) -> Self {
        Self {
            binders: Vec::new(),
            body,
        }
    }

    pub fn binders(&self) -> &[InferVarId] {
        &self.binders
    }

    pub fn body(&self) -> &Type {
        &self.body
    }

    pub fn instantiate(&self, table: &mut crate::core::unification::UnificationTable) -> InferTy {
        let remap = self
            .binders
            .iter()
            .map(|binder| (binder.0, table.fresh_var()))
            .collect();
        let mut folder = crate::core::type_folder::RemapFolder::new(remap);
        InferTy::from_type(crate::core::type_folder::walk_type(
            self.body.clone(),
            &mut folder,
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckedConversionKind {
    Identity,
    NumericWiden,
    TraitUpcast,
    DynamicPack,
    DynamicDowncast,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CheckedConversion {
    pub kind: CheckedConversionKind,
    pub from: ZonkedTy,
    pub to: ZonkedTy,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedInstantiation {
    pub scheme_owner: crate::core::NodeId,
    pub substitutions: Vec<(String, ZonkedTy)>,
}

impl From<BackendTy> for Type {
    fn from(ty: BackendTy) -> Self {
        ty.0
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn contains_infer_artifacts(ty: &Type) -> bool {
    match ty {
        Type::Located { ty, .. } => contains_infer_artifacts(ty),
        Type::TypeVar(_) | Type::ForAll(..) | Type::Infer => true,
        Type::Name(n, _) if n == "_" || n == "unknown" => true,
        Type::Name(_, args) => args.iter().any(contains_infer_artifacts),
        Type::Option(inner) => contains_infer_artifacts(inner),
        Type::Result(ok, err) => contains_infer_artifacts(ok) || contains_infer_artifacts(err),
        Type::Tuple(elems) => elems.iter().any(contains_infer_artifacts),
        Type::Func(args, ret) | Type::ExternFunc(args, ret) => {
            args.iter().any(contains_infer_artifacts) || contains_infer_artifacts(ret)
        }
        Type::Ref(_, inner)
        | Type::RefMut(_, inner)
        | Type::Shared(inner)
        | Type::LocalShared(inner)
        | Type::Weak(inner)
        | Type::WeakLocal(inner)
        | Type::RawPtr(inner)
        | Type::RawPtrMut(inner)
        | Type::CShared(inner)
        | Type::CBorrow(inner)
        | Type::CBorrowMut(inner)
        | Type::CBuffer(inner)
        | Type::Array(inner, _)
        | Type::Slice(inner)
        | Type::Newtype(_, inner) => contains_infer_artifacts(inner),
        Type::Cap(_)
        | Type::ImplTrait(_)
        | Type::DynTrait(_)
        | Type::Nothing
        | Type::Allocator
        | Type::RawString => false,
    }
}

fn contains_inference_variables(ty: &Type) -> bool {
    match ty {
        Type::Located { ty, .. } => contains_inference_variables(ty),
        Type::TypeVar(_) | Type::ForAll(..) => true,
        Type::Name(_, args) | Type::Tuple(args) => args.iter().any(contains_inference_variables),
        Type::Result(ok, err) => {
            contains_inference_variables(ok) || contains_inference_variables(err)
        }
        Type::Func(args, ret) | Type::ExternFunc(args, ret) => {
            args.iter().any(contains_inference_variables) || contains_inference_variables(ret)
        }
        Type::Ref(_, inner)
        | Type::RefMut(_, inner)
        | Type::Option(inner)
        | Type::Shared(inner)
        | Type::LocalShared(inner)
        | Type::Weak(inner)
        | Type::WeakLocal(inner)
        | Type::RawPtr(inner)
        | Type::RawPtrMut(inner)
        | Type::CShared(inner)
        | Type::CBorrow(inner)
        | Type::CBorrowMut(inner)
        | Type::CBuffer(inner)
        | Type::Array(inner, _)
        | Type::Slice(inner)
        | Type::Newtype(_, inner) => contains_inference_variables(inner),
        Type::Infer
        | Type::Nothing
        | Type::Allocator
        | Type::RawString
        | Type::Cap(_)
        | Type::ImplTrait(_)
        | Type::DynTrait(_) => false,
    }
}

fn contains_dynamic_type(ty: &Type) -> bool {
    match ty {
        Type::Located { ty, .. } => contains_dynamic_type(ty),
        Type::Name(name, args) => name == "Any" || args.iter().any(contains_dynamic_type),
        Type::Result(ok, err) => contains_dynamic_type(ok) || contains_dynamic_type(err),
        Type::Tuple(items) => items.iter().any(contains_dynamic_type),
        Type::Func(args, ret) | Type::ExternFunc(args, ret) => {
            args.iter().any(contains_dynamic_type) || contains_dynamic_type(ret)
        }
        Type::Ref(_, inner)
        | Type::RefMut(_, inner)
        | Type::Option(inner)
        | Type::Shared(inner)
        | Type::LocalShared(inner)
        | Type::Weak(inner)
        | Type::WeakLocal(inner)
        | Type::RawPtr(inner)
        | Type::RawPtrMut(inner)
        | Type::CShared(inner)
        | Type::CBorrow(inner)
        | Type::CBorrowMut(inner)
        | Type::CBuffer(inner)
        | Type::Array(inner, _)
        | Type::Slice(inner)
        | Type::Newtype(_, inner)
        | Type::ForAll(_, inner) => contains_dynamic_type(inner),
        Type::Infer
        | Type::TypeVar(_)
        | Type::Nothing
        | Type::Allocator
        | Type::RawString
        | Type::Cap(_)
        | Type::ImplTrait(_)
        | Type::DynTrait(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn i32_ty() -> Type {
        Type::Name("i32".into(), vec![])
    }

    #[test]
    fn surface_to_infer_roundtrip() {
        let s = SurfaceTy::new(i32_ty()).unwrap();
        let i = s.into_infer();
        assert_eq!(i.0, i32_ty());
    }

    #[test]
    fn infer_into_surface_ok() {
        let i = InferTy::from_type(i32_ty());
        assert!(i.into_surface().is_ok());
    }

    #[test]
    fn infer_into_surface_rejects_typevar() {
        let i = InferTy::from_type(Type::TypeVar(0));
        assert!(i.into_surface().is_err());
    }

    #[test]
    fn infer_into_surface_rejects_forall() {
        let i = InferTy::from_type(Type::ForAll(vec!["T".into()], Box::new(Type::TypeVar(0))));
        assert!(i.into_surface().is_err());
    }

    #[test]
    fn zonked_from_resolved_ok() {
        assert!(ZonkedTy::from_resolved(i32_ty()).is_ok());
    }

    #[test]
    fn zonked_from_resolved_rejects_typevar() {
        assert!(ZonkedTy::from_resolved(Type::TypeVar(0)).is_err());
    }

    #[test]
    fn zonked_from_resolved_rejects_infer() {
        assert!(ZonkedTy::from_resolved(Type::Infer).is_err());
    }

    #[test]
    fn zonked_from_resolved_rejects_forall() {
        assert!(ZonkedTy::from_resolved(Type::ForAll(vec![], Box::new(i32_ty()))).is_err());
    }

    #[test]
    fn zonked_from_resolved_rejects_underscore() {
        assert!(ZonkedTy::from_resolved(Type::Name("_".into(), vec![])).is_err());
    }

    #[test]
    fn expression_zonk_completes_uninhabited_variant_slots() {
        let mut table = crate::core::unification::UnificationTable::new();
        let unresolved = Type::Result(
            Box::new(i32_ty()),
            Box::new(Type::TypeVar(table.fresh_var())),
        );
        let completed = ZonkedTy::from_expression_type(&unresolved, &mut table).unwrap();
        assert_eq!(
            completed.as_type(),
            &Type::Result(Box::new(i32_ty()), Box::new(Type::Nothing))
        );
    }

    #[test]
    fn expression_zonk_still_rejects_unknown_escape() {
        let mut table = crate::core::unification::UnificationTable::new();
        assert!(ZonkedTy::from_expression_type(
            &Type::Name("unknown".into(), Vec::new()),
            &mut table,
        )
        .is_err());
    }

    #[test]
    fn backend_from_zonked_identity() {
        let z = ZonkedTy::from_resolved(i32_ty()).unwrap();
        let b = BackendTy::from_zonked(z, &BackendProfile::Interpreter).unwrap();
        assert_eq!(b.0, i32_ty());
    }

    #[test]
    fn backend_dynamic_type_requires_interpreter_profile() {
        let dynamic = ZonkedTy::from_resolved(Type::Name("Any".into(), Vec::new())).unwrap();
        assert!(BackendTy::from_zonked(dynamic.clone(), &BackendProfile::Interpreter).is_ok());
        assert!(BackendTy::from_zonked(dynamic, &BackendProfile::Native).is_err());
    }

    #[test]
    fn scheme_instantiation_is_fresh_and_alpha_equivalent() {
        let scheme = TypeScheme::new(
            vec![InferVarId(0)],
            Type::Func(vec![Type::TypeVar(0)], Box::new(Type::TypeVar(0))),
        )
        .unwrap();
        let mut table = crate::core::unification::UnificationTable::new();
        let first = scheme.instantiate(&mut table);
        let second = scheme.instantiate(&mut table);
        assert_ne!(first.as_type(), second.as_type());
        assert!(matches!(
            (first.as_type(), second.as_type()),
            (Type::Func(_, _), Type::Func(_, _))
        ));
    }
}
