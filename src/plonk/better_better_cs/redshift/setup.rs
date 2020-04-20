use crate::pairing::{Engine};
use crate::pairing::ff::{Field, PrimeField, PrimeFieldRepr};
use crate::worker::Worker;
use crate::plonk::commitments::transparent::utils::log2_floor;
use super::*;
use super::tree_hash::*;
use super::binary_tree::{BinaryTree, BinaryTreeParams};
use crate::plonk::polynomials::*;
use super::multioracle::Multioracle;
use super::super::cs::*;
use crate::SynthesisError;

pub struct SetupMultioracle<E: Engine, H: BinaryTreeHasher<E::Fr>> {
    pub polynomials_in_monomial_form: Vec<Polynomial<E::Fr, Coefficients>>,
    pub polynomial_ldes: Vec<Polynomial<E::Fr, Values>>,
    pub setup_ids: Vec<PolyIdentifier>,
    pub permutations_ranges: Vec<std::ops::Range<usize>>,
    pub gate_selectors_indexes: Vec<usize>,
    pub tree: BinaryTree<E, H>
}

pub const LDE_FACTOR: usize = 8;
pub const FRI_VALUES_PER_LEAF: usize = 8;

impl<E: Engine, H: BinaryTreeHasher<E::Fr>> SetupMultioracle<E, H> {
    pub fn from_assembly<P: PlonkConstraintSystemParams<E>, MG: MainGateEquation>(
        assembly: TrivialAssembly<E, P, MG>,
        tree_hasher: H,
        worker: &Worker
    ) -> Result<(Self, Vec<Polynomial<E::Fr, Values>>), SynthesisError> {
        use crate::plonk::fft::cooley_tukey_ntt::*;

        let size = assembly.n().next_power_of_two();

        let (mut storage, permutations) = assembly.perform_setup(&worker)?;
        let gate_selectors = assembly.output_gate_selectors(&worker)?;
        let ids = assembly.sorted_setup_polynomial_ids.clone();
        println!("IDs = {:?}", ids);
        drop(assembly);

        let mut setup_polys = vec![];

        let mut mononial_forms = vec![];

        let omegas_bitreversed = BitReversedOmegas::<E::Fr>::new_for_domain_size(size.next_power_of_two());
        let omegas_inv_bitreversed = <OmegasInvBitreversed::<E::Fr> as CTPrecomputations::<E::Fr>>::new_for_domain_size(size.next_power_of_two());
    
        for id in ids.iter() {
            let mut setup_poly = storage.remove(&id).expect(&format!("must contain a poly for id {:?}", id));
            setup_poly.pad_to_domain()?;
            let coeffs = setup_poly.ifft_using_bitreversed_ntt(&worker, &omegas_inv_bitreversed, &E::Fr::one())?;
            mononial_forms.push(coeffs.clone());
            let lde = coeffs.bitreversed_lde_using_bitreversed_ntt(&worker, LDE_FACTOR, &omegas_bitreversed, &E::Fr::multiplicative_generator())?;

            setup_polys.push(lde);
        }

        let mut permutations_ranges = vec![];
        let before = setup_polys.len();

        for mut p in permutations.iter().cloned() {
            p.pad_to_domain()?;
            let coeffs = p.ifft_using_bitreversed_ntt(&worker, &omegas_inv_bitreversed, &E::Fr::one())?;
            mononial_forms.push(coeffs.clone());
            let lde = coeffs.bitreversed_lde_using_bitreversed_ntt(&worker, LDE_FACTOR, &omegas_bitreversed, &E::Fr::multiplicative_generator())?;

            setup_polys.push(lde);
        }

        let after = setup_polys.len();

        permutations_ranges.push(before..after);

        let mut gate_selectors_indexes = vec![];

        for mut selector in gate_selectors.into_iter() {
            let before = setup_polys.len();
            gate_selectors_indexes.push(before);

            selector.pad_to_domain()?;
            let coeffs = selector.ifft_using_bitreversed_ntt(&worker, &omegas_inv_bitreversed, &E::Fr::one())?;
            mononial_forms.push(coeffs.clone());
            let lde = coeffs.bitreversed_lde_using_bitreversed_ntt(&worker, LDE_FACTOR, &omegas_bitreversed, &E::Fr::multiplicative_generator())?;

            setup_polys.push(lde);
        }

        let multioracle = Multioracle::<E, H>::new_from_polynomials(
            &setup_polys, 
            tree_hasher, 
            FRI_VALUES_PER_LEAF,
            &worker
        );

        let tree = multioracle.tree;

        let setup = Self {
            polynomials_in_monomial_form: mononial_forms,
            polynomial_ldes: setup_polys,
            tree,
            setup_ids: ids,
            permutations_ranges,
            gate_selectors_indexes,
        };

        Ok((setup, permutations))
    }
}

