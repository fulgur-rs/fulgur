//! Consumer-selected fallback for unavailable ordinary image resources.

use raikiri_traits::{
    IntrinsicBox, ReplacedResolver, ResolveDisposition, ResolvedIntrinsic, ResolverError,
    ResolverRequest,
};

pub(crate) struct OptionalImages<'a, R>(pub(crate) &'a R);

impl<R: ReplacedResolver> ReplacedResolver for OptionalImages<'_, R> {
    fn resolve(
        &self,
        request: ResolverRequest<'_>,
    ) -> std::result::Result<ResolvedIntrinsic, ResolverError> {
        // The consumer intentionally keeps rendering a document whose local
        // image is unavailable. Raikiri records the explicit fallback warning.
        Ok(self
            .0
            .resolve(request)
            .unwrap_or_else(|error| ResolvedIntrinsic {
                intrinsic: IntrinsicBox::new(0.0, 0.0),
                disposition: ResolveDisposition::Fallback {
                    reason: error.to_string(),
                },
            }))
    }
}
