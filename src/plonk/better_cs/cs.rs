use crate::pairing::ff::{Field};
use crate::pairing::{Engine};

use crate::{SynthesisError};
use std::marker::PhantomData;

pub use crate::plonk::cs::variable::*;

pub trait Circuit<E: Engine, G: Copy + Clone + PartialEq + Eq> {
    fn synthesize<CS: ConstraintSystem<E, GateCoefficients = G>>(&self, cs: &mut CS) -> Result<(), SynthesisError>;
}

pub trait ConstraintSystem<E: Engine> {
    type GateCoefficients: Copy + Clone + PartialEq + Eq;

    // allocate a variable
    fn alloc<F>(&mut self, value: F) -> Result<Variable, SynthesisError>
    where
        F: FnOnce() -> Result<E::Fr, SynthesisError>;

    // allocate an input variable
    fn alloc_input<F>(&mut self, value: F) -> Result<Variable, SynthesisError>
    where
        F: FnOnce() -> Result<E::Fr, SynthesisError>;

    fn new_gate(&mut self, variables: (Variable, Variable, Variable), 
        coeffs: Self::GateCoefficients) -> Result<(), SynthesisError>;

    fn get_value(&self, _variable: Variable) -> Result<E::Fr, SynthesisError> { 
        Err(SynthesisError::AssignmentMissing)
    }

    fn get_dummy_variable(&self) -> Variable;
}
