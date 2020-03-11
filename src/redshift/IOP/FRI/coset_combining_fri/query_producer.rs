use crate::pairing::ff::PrimeField;
use crate::redshift::polynomials::*;
use crate::redshift::domains::*;
use crate::multicore::*;
use crate::SynthesisError;
use super::fri::*;
use super::*;
use crate::redshift::IOP::oracle::*;

impl<F: PrimeField, I: Oracle<F>> FriProofPrototype<F, I>
{
    pub fn produce_proof(
        self,
        natural_first_element_indexes: Vec<usize>,
    ) -> Result<FriProof<F, I>, SynthesisError> {

        let domain_size = self.initial_degree_plus_one * self.lde_factor;
        let mut commitments = vec![];

        for iop in &self.oracles {
            commitments.push(iop.get_commitment());
        }

        let mut rounds = vec![];

        for natural_first_element_index in natural_first_element_indexes.into_iter() {
            let mut queries = vec![];
            let mut domain_idx = natural_first_element_index;
            let mut domain_size = domain_size;

            for (commitment, leaf_values) in self.intermediate_commitments.into_iter()
                                        .zip(Some(iop_values).into_iter().chain(&self.intermediate_values)) {
                
                let coset_values = <I::Combiner as CosetCombiner<F>>::get_coset_for_natural_index(domain_idx, domain_size);
                if coset_values.len() != <I::Combiner as CosetCombiner<F>>::COSET_SIZE {
                    return Err(SynthesisError::PolynomialDegreeTooLarge);
                }

                for idx in coset_values.into_iter() {
                    let query = iop.query(idx, leaf_values.as_ref());
                    queries.push(query);
                }

                let (next_idx, next_size) = Domain::<F>::index_and_size_for_next_domain(domain_idx, domain_size);

                domain_idx = next_idx;
                domain_size = next_size;
            }

            rounds.push(queries);
        }

        let proof = FriProof::<F, I> {
            queries: rounds,
            commitments,
            final_coefficients: self.final_coefficients,
            initial_degree_plus_one: self.initial_degree_plus_one,
            output_coeffs_at_degree_plus_one: self.output_coeffs_at_degree_plus_one,
            lde_factor: self.lde_factor,
        };

        Ok(proof)
    }
}