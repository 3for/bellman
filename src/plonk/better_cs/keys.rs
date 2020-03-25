use super::cs::*;

use crate::pairing::ff::{Field, PrimeField};
use crate::pairing::{Engine};

use crate::{SynthesisError};
use crate::plonk::polynomials::*;
use crate::worker::Worker;
use crate::plonk::domains::*;

use crate::kate_commitment::*;

use std::marker::PhantomData;

use super::utils::*;
use super::LDE_FACTOR;

pub struct SetupPolynomials<E: Engine, P: PlonkConstraintSystemParams<E>> {
    pub n: usize,
    pub num_inputs: usize,
    pub selector_polynomials: Vec<Polynomial<E::Fr, Coefficients>>,
    pub next_step_selector_polynomials: Vec<Polynomial<E::Fr, Coefficients>>,
    pub permutation_polynomials: Vec<Polynomial<E::Fr, Coefficients>>,

    pub(crate) _marker: std::marker::PhantomData<P>
}

use std::io::{Read, Write};
use crate::byteorder::ReadBytesExt;
use crate::byteorder::WriteBytesExt;
use crate::byteorder::BigEndian;

pub fn write_fr<F: PrimeField, W: Write>(
    el: &F,
    mut writer: W
) -> std::io::Result<()> {
    use crate::ff::PrimeFieldRepr;

    let repr = el.into_repr();
    repr.write_be(&mut writer)?;

    Ok(())
}

pub fn write_fr_raw<F: PrimeField, W: Write>(
    el: &F,
    mut writer: W
) -> std::io::Result<()> {
    use crate::ff::PrimeFieldRepr;

    let repr = el.into_raw_repr();
    repr.write_be(&mut writer)?;

    Ok(())
}

pub fn read_fr<F: PrimeField, R: Read>(
    mut reader: R
) -> std::io::Result<F> {
    use crate::ff::PrimeFieldRepr;

    let mut repr = F::Repr::default();
    repr.read_be(&mut reader)?;

    F::from_repr(repr).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

pub fn read_fr_raw<F: PrimeField, R: Read>(
    mut reader: R
) -> std::io::Result<F> {
    use crate::ff::PrimeFieldRepr;
    let mut repr = F::Repr::default();
    repr.read_be(&mut reader)?;

    F::from_raw_repr(repr).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

impl<E: Engine, P: PlonkConstraintSystemParams<E>> SetupPolynomials<E, P> {
    pub fn write<W: Write>(
        &self,
        mut writer: W
    ) -> std::io::Result<()>
    {
        writer.write_u64::<BigEndian>(self.n as u64)?;
        writer.write_u64::<BigEndian>(self.num_inputs as u64)?;

        writer.write_u64::<BigEndian>(self.selector_polynomials.len() as u64)?;
        for p in self.selector_polynomials.iter() {
            writer.write_u64::<BigEndian>(p.as_ref().len() as u64)?;
            for el in p.as_ref().iter() {
                write_fr(el, &mut writer)?;
            }
        }

        writer.write_u64::<BigEndian>(self.next_step_selector_polynomials.len() as u64)?;
        for p in self.next_step_selector_polynomials.iter() {
            writer.write_u64::<BigEndian>(p.as_ref().len() as u64)?;
            for el in p.as_ref().iter() {
                write_fr(el, &mut writer)?;
            }
        }

        writer.write_u64::<BigEndian>(self.permutation_polynomials.len() as u64)?;
        for p in self.permutation_polynomials.iter() {
            writer.write_u64::<BigEndian>(p.as_ref().len() as u64)?;
            for el in p.as_ref().iter() {
                write_fr(el, &mut writer)?;
            }
        }

        Ok(())
    }

    pub fn read<R: Read>(
        mut reader: R
    ) -> std::io::Result<Self>
    {
        let n = reader.read_u64::<BigEndian>()?;
        let num_inputs = reader.read_u64::<BigEndian>()?;

        let num_selectors = reader.read_u64::<BigEndian>()?;
        let mut selectors = Vec::with_capacity(num_selectors as usize);
        for _ in 0..num_selectors {
            let num_values = reader.read_u64::<BigEndian>()?;
            let mut poly_coeffs = Vec::with_capacity(num_values as usize);
            for _ in 0..num_values {
                let el = read_fr(&mut reader)?;
                poly_coeffs.push(el);
            }

            let poly = Polynomial::from_coeffs(poly_coeffs).expect("must fit into some domain");
            selectors.push(poly);
        }

        let num_next_step_selectors = reader.read_u64::<BigEndian>()?;
        let mut next_step_selectors = Vec::with_capacity(num_next_step_selectors as usize);
        for _ in 0..num_selectors {
            let num_values = reader.read_u64::<BigEndian>()?;
            let mut poly_coeffs = Vec::with_capacity(num_values as usize);
            for _ in 0..num_values {
                let el = read_fr(&mut reader)?;
                poly_coeffs.push(el);
            }

            let poly = Polynomial::from_coeffs(poly_coeffs).expect("must fit into some domain");
            next_step_selectors.push(poly);
        }

        let num_permutation_polys = reader.read_u64::<BigEndian>()?;
        let mut permutation_polys = Vec::with_capacity(num_permutation_polys as usize);
        for _ in 0..num_selectors {
            let num_values = reader.read_u64::<BigEndian>()?;
            let mut poly_coeffs = Vec::with_capacity(num_values as usize);
            for _ in 0..num_values {
                let el = read_fr(&mut reader)?;
                poly_coeffs.push(el);
            }

            let poly = Polynomial::from_coeffs(poly_coeffs).expect("must fit into some domain");
            permutation_polys.push(poly);
        }

        let new = Self{
            n: n as usize,
            num_inputs: num_inputs as usize,
            selector_polynomials: selectors,
            next_step_selector_polynomials: next_step_selectors,
            permutation_polynomials: permutation_polys,
        
            _marker: std::marker::PhantomData
        };

        Ok(new)
    }  
}

pub struct SetupPolynomialsPrecomputations<E: Engine, P: PlonkConstraintSystemParams<E>> {
    pub selector_polynomials_on_coset_of_size_4n_bitreversed: Vec<Polynomial<E::Fr, Values>>,
    pub next_step_selector_polynomials_on_coset_of_size_4n_bitreversed: Vec<Polynomial<E::Fr, Values>>,
    pub permutation_polynomials_on_coset_of_size_4n_bitreversed: Vec<Polynomial<E::Fr, Values>>,
    pub permutation_polynomials_values_of_size_n_minus_one: Vec<Polynomial<E::Fr, Values>>,
    pub inverse_divisor_on_coset_of_size_4n_bitreversed: Polynomial<E::Fr, Values>,
    pub x_on_coset_of_size_4n_bitreversed: Polynomial<E::Fr, Values>,

    pub(crate) _marker: std::marker::PhantomData<P>
}

use crate::plonk::fft::cooley_tukey_ntt::{BitReversedOmegas, CTPrecomputations};

impl<E: Engine, P: PlonkConstraintSystemParams<E>> SetupPolynomialsPrecomputations<E, P> {
    pub fn from_setup_and_precomputations<CP: CTPrecomputations<E::Fr>> (
        setup: &SetupPolynomials<E, P>,
        worker: &Worker,
        omegas_bitreversed: &CP,
    ) -> Result<Self, SynthesisError> {
        let mut new = Self {
            selector_polynomials_on_coset_of_size_4n_bitreversed: vec![],
            next_step_selector_polynomials_on_coset_of_size_4n_bitreversed: vec![],
            permutation_polynomials_on_coset_of_size_4n_bitreversed: vec![],
            permutation_polynomials_values_of_size_n_minus_one: vec![],
            inverse_divisor_on_coset_of_size_4n_bitreversed: Polynomial::from_values(vec![E::Fr::one()]).unwrap(),
            x_on_coset_of_size_4n_bitreversed: Polynomial::from_values(vec![E::Fr::one()]).unwrap(),
            
            _marker: std::marker::PhantomData
        };

        let required_domain_size = setup.selector_polynomials[0].size();

        assert!(required_domain_size.is_power_of_two());
        let coset_generator = E::Fr::multiplicative_generator();

        // let coset_generator = E::Fr::one();

        // we do not precompute q_const as we need to use it for public inputs;
        for p in setup.selector_polynomials[0..(setup.selector_polynomials.len() - 1)].iter() {
            let ext = p.clone().bitreversed_lde_using_bitreversed_ntt(
                &worker, 
                LDE_FACTOR, 
                omegas_bitreversed, 
                &coset_generator
            )?;

            new.selector_polynomials_on_coset_of_size_4n_bitreversed.push(ext);
        }

        for p in setup.next_step_selector_polynomials.iter() {
            let ext = p.clone().bitreversed_lde_using_bitreversed_ntt(
                &worker, 
                LDE_FACTOR, 
                omegas_bitreversed, 
                &coset_generator
            )?;

            new.next_step_selector_polynomials_on_coset_of_size_4n_bitreversed.push(ext);
        }

        for p in setup.permutation_polynomials.iter() {
            let lde = p.clone().bitreversed_lde_using_bitreversed_ntt(
                &worker, 
                LDE_FACTOR, 
                omegas_bitreversed, 
                &coset_generator
            )?;
            new.permutation_polynomials_on_coset_of_size_4n_bitreversed.push(lde);

            let as_values = p.clone().fft(&worker);
            let mut as_values = as_values.into_coeffs();
            as_values.pop().expect("must shorted permutation polynomial values by one");

            let p = Polynomial::from_values_unpadded(as_values)?;

            new.permutation_polynomials_values_of_size_n_minus_one.push(p);
        }
        
        let mut vanishing_poly_inverse_bitreversed = evaluate_vanishing_polynomial_of_degree_on_domain_size::<E::Fr>(
            required_domain_size as u64, 
            &E::Fr::multiplicative_generator(),
            (required_domain_size * LDE_FACTOR) as u64,
            &worker, 
        )?;
        vanishing_poly_inverse_bitreversed.batch_inversion(&worker)?;
        vanishing_poly_inverse_bitreversed.bitreverse_enumeration(&worker);

        assert_eq!(vanishing_poly_inverse_bitreversed.size(), required_domain_size * LDE_FACTOR);

        // evaluate polynomial X on the coset
        let mut x_poly = Polynomial::from_values(vec![coset_generator; vanishing_poly_inverse_bitreversed.size()])?;
        x_poly.distribute_powers(&worker, x_poly.omega);
        x_poly.bitreverse_enumeration(&worker);

        assert_eq!(x_poly.size(), required_domain_size * LDE_FACTOR);

        new.inverse_divisor_on_coset_of_size_4n_bitreversed = vanishing_poly_inverse_bitreversed;
        new.x_on_coset_of_size_4n_bitreversed = x_poly;

        Ok(new)
    }

    pub fn from_setup (
        setup: &SetupPolynomials<E, P>,
        worker: &Worker,
    ) -> Result<Self, SynthesisError> {
        let precomps = BitReversedOmegas::new_for_domain_size(setup.permutation_polynomials[0].size());

        Self::from_setup_and_precomputations(
            setup, 
            worker, 
            &precomps
        )  
    }
}

#[derive(Clone, Debug)]
pub struct Proof<E: Engine, P: PlonkConstraintSystemParams<E>> {
    pub num_inputs: usize,
    pub n: usize,
    pub input_values: Vec<E::Fr>,
    pub wire_commitments: Vec<E::G1Affine>,
    pub grand_product_commitment: E::G1Affine,
    pub quotient_poly_commitments: Vec<E::G1Affine>,

    pub wire_values_at_z: Vec<E::Fr>,
    pub wire_values_at_z_omega: Vec<E::Fr>,
    pub grand_product_at_z_omega: E::Fr,
    pub quotient_polynomial_at_z: E::Fr,
    pub linearization_polynomial_at_z: E::Fr,
    pub permutation_polynomials_at_z: Vec<E::Fr>,

    pub opening_at_z_proof: E::G1Affine,
    pub opening_at_z_omega_proof: E::G1Affine,

    pub(crate) _marker: std::marker::PhantomData<P>
}

impl<E: Engine, P: PlonkConstraintSystemParams<E>> Proof<E, P> {
    pub fn empty() -> Self {
        use crate::pairing::CurveAffine;

        Self {
            num_inputs: 0,
            n: 0,
            input_values: vec![],
            wire_commitments: vec![],
            grand_product_commitment: E::G1Affine::zero(),
            quotient_poly_commitments: vec![],
            wire_values_at_z: vec![],
            wire_values_at_z_omega: vec![],
            grand_product_at_z_omega: E::Fr::zero(),
            quotient_polynomial_at_z: E::Fr::zero(),
            linearization_polynomial_at_z: E::Fr::zero(),
            permutation_polynomials_at_z: vec![],

            opening_at_z_proof: E::G1Affine::zero(),
            opening_at_z_omega_proof: E::G1Affine::zero(),

            _marker: std::marker::PhantomData
        }
    }
}

#[derive(Clone, Debug)]
pub struct VerificationKey<E: Engine, P: PlonkConstraintSystemParams<E>> {
    pub n: usize,
    pub num_inputs: usize,
    pub selector_commitments: Vec<E::G1Affine>,
    pub next_step_selector_commitments: Vec<E::G1Affine>,
    pub permutation_commitments: Vec<E::G1Affine>,

    pub g2_elements: [E::G2Affine; 2],

    pub(crate) _marker: std::marker::PhantomData<P>
}

impl<E: Engine, P: PlonkConstraintSystemParams<E>> VerificationKey<E, P> {
    pub fn from_setup(
        setup: &SetupPolynomials<E, P>, 
        worker: &Worker, 
        crs: &Crs<E, CrsForMonomialForm>
    ) -> Result<Self, SynthesisError> {
        assert_eq!(setup.selector_polynomials.len(), P::STATE_WIDTH + 2);
        if P::CAN_ACCESS_NEXT_TRACE_STEP == false {
            assert_eq!(setup.next_step_selector_polynomials.len(), 0);
        }
        assert_eq!(setup.permutation_polynomials.len(), P::STATE_WIDTH);

        let mut new = Self {
            n: setup.n,
            num_inputs: setup.num_inputs,
            selector_commitments: vec![],
            next_step_selector_commitments: vec![],
            permutation_commitments: vec![],

            g2_elements: [crs.g2_monomial_bases[0], crs.g2_monomial_bases[1]],
        
            _marker: std::marker::PhantomData
        };

        for p in setup.selector_polynomials.iter() {
            let commitment = commit_using_monomials(p, &crs, &worker)?;
            new.selector_commitments.push(commitment);
        }

        for p in setup.next_step_selector_polynomials.iter() {
            let commitment = commit_using_monomials(p, &crs, &worker)?;
            new.next_step_selector_commitments.push(commitment);
        }

        for p in setup.permutation_polynomials.iter() {
            let commitment = commit_using_monomials(p, &crs, &worker)?;
            new.permutation_commitments.push(commitment);
        }

        Ok(new)
    }
}



