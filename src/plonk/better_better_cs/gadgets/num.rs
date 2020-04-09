use crate::pairing::{
    Engine,
};

use crate::pairing::ff::{
    Field,
    PrimeField,
    PrimeFieldRepr,
    BitIterator
};

use crate::{
    SynthesisError,
};

use crate::plonk::better_better_cs::cs::{Variable, ConstraintSystem, PlonkConstraintSystemParams, MainGateEquation};

use super::assignment::{
    Assignment
};

use std::ops::{Add, Sub};

pub struct AllocatedNum<E: Engine> {
    value: Option<E::Fr>,
    variable: Variable
}

impl<E: Engine> Clone for AllocatedNum<E> {
    fn clone(&self) -> Self {
        AllocatedNum {
            value: self.value,
            variable: self.variable
        }
    }
}

impl<E: Engine> AllocatedNum<E> {
    pub fn alloc<P, MG, CS, F>(
        mut cs: CS,
        value: F,
    ) -> Result<Self, SynthesisError>
        where P: PlonkConstraintSystemParams<E>,
            MG: MainGateEquation,
            CS: ConstraintSystem<E, P, MG>,
            F: FnOnce() -> Result<E::Fr, SynthesisError>
    {
        let mut new_value = None;
        let var = cs.alloc(
            || {
                let tmp = value()?;

                new_value = Some(tmp);

                Ok(tmp)
            }
        )?;

        Ok(AllocatedNum {
            value: new_value,
            variable: var
        })
    }

    pub fn add<CS>(
        &self,
        mut cs: CS,
        other: &Self
    ) -> Result<Self, SynthesisError>
        where CS: ConstraintSystem<E>
    {
        let mut value = None;

        let var = cs.alloc(|| "add num", || {
            let mut tmp = *self.value.get()?;
            tmp.add_assign(other.value.get()?);

            value = Some(tmp);

            Ok(tmp)
        })?;

        // Constrain: a * b = ab
        cs.enforce(
            || "addition constraint",
            |zero| zero + self.variable + other.variable,
            |zero| zero + CS::one(),
            |zero| zero + var
        );

        Ok(AllocatedNum {
            value: value,
            variable: var
        })
    }

    pub fn add_constant<CS>(
        &self,
        mut cs: CS,
        constant: E::Fr
    ) -> Result<Self, SynthesisError>
        where CS: ConstraintSystem<E>
    {
        let mut value = None;

        let var = cs.alloc(|| "add constant to num", || {
            let mut tmp = *self.value.get()?;
            tmp.add_assign(&constant);

            value = Some(tmp);

            Ok(tmp)
        })?;

        // Constrain: a * b = ab
        cs.enforce(
            || "addition constraint",
            |zero| zero + self.variable + (constant, CS::one()),
            |zero| zero + CS::one(),
            |zero| zero + var
        );

        Ok(AllocatedNum {
            value: value,
            variable: var
        })
    }

    pub fn sub<CS>(
        &self,
        mut cs: CS,
        other: &Self
    ) -> Result<Self, SynthesisError>
        where CS: ConstraintSystem<E>
    {
        let mut value = None;

        let var = cs.alloc(|| "sub num", || {
            let mut tmp = *self.value.get()?;
            tmp.sub_assign(other.value.get()?);

            value = Some(tmp);

            Ok(tmp)
        })?;

        // Constrain: a * b = ab
        cs.enforce(
            || "addition constraint",
            |zero| zero + self.variable - other.variable,
            |zero| zero + CS::one(),
            |zero| zero + var
        );

        Ok(AllocatedNum {
            value: value,
            variable: var
        })
    }

    pub fn mul<CS>(
        &self,
        mut cs: CS,
        other: &Self
    ) -> Result<Self, SynthesisError>
        where CS: ConstraintSystem<E>
    {
        let mut value = None;

        let var = cs.alloc(|| "product num", || {
            let mut tmp = *self.value.get()?;
            tmp.mul_assign(other.value.get()?);

            value = Some(tmp);

            Ok(tmp)
        })?;

        // Constrain: a * b = ab
        cs.enforce(
            || "multiplication constraint",
            |zero| zero + self.variable,
            |zero| zero + other.variable,
            |zero| zero + var
        );

        Ok(AllocatedNum {
            value: value,
            variable: var
        })
    }

    pub fn square<CS>(
        &self,
        mut cs: CS,
    ) -> Result<Self, SynthesisError>
        where CS: ConstraintSystem<E>
    {
        let mut value = None;

        let var = cs.alloc(|| "squared num", || {
            let mut tmp = *self.value.get()?;
            tmp.square();

            value = Some(tmp);

            Ok(tmp)
        })?;

        // Constrain: a * a = aa
        cs.enforce(
            || "squaring constraint",
            |zero| zero + self.variable,
            |zero| zero + self.variable,
            |zero| zero + var
        );

        Ok(AllocatedNum {
            value: value,
            variable: var
        })
    }
    
    pub fn pow<CS>(
        &self,
        mut cs: CS,
        power: &E::Fr
    )-> Result<Self, SynthesisError>
        where CS: ConstraintSystem<E>
    {
        let power_bits: Vec<bool> = BitIterator::new(power.into_repr()).collect();
        let mut temp = AllocatedNum::alloc(cs.namespace(||"one"), ||Ok(E::Fr::one()))?;
        temp.assert_number(cs.namespace(||"assert_one"), &E::Fr::one())?;
        
        for (i, bit) in power_bits.iter().enumerate(){
            temp = temp.square(cs.namespace(||format!("square on step: {}", i)))?;
            if *bit{
                temp = temp.mul(cs.namespace(||format!("mul step: {}", i)), &self)?;
            }
        };

        Ok(temp)
    }

    pub fn assert_nonzero<CS>(
        &self,
        mut cs: CS
    ) -> Result<(), SynthesisError>
        where CS: ConstraintSystem<E>
    {
        let inv = cs.alloc(|| "ephemeral inverse", || {
            let tmp = *self.value.get()?;
            
            if tmp.is_zero() {
                Err(SynthesisError::DivisionByZero)
            } else {
                Ok(tmp.inverse().unwrap())
            }
        })?;

        // Constrain a * inv = 1, which is only valid
        // iff a has a multiplicative inverse, untrue
        // for zero.
        cs.enforce(
            || "nonzero assertion constraint",
            |zero| zero + self.variable,
            |zero| zero + inv,
            |zero| zero + CS::one()
        );

        Ok(())
    }

    pub fn assert_zero<CS>(
        &self,
        mut cs: CS
    ) -> Result<(), SynthesisError>
        where CS: ConstraintSystem<E>
    {
        cs.enforce(
            || "zero assertion constraint",
            |zero| zero + self.variable,
            |zero| zero + CS::one(),
            |zero| zero
        );

        Ok(())
    }

    pub fn assert_number<CS>(
        &self,
        mut cs: CS,
        number: &E::Fr
    ) -> Result<(), SynthesisError>
        where CS: ConstraintSystem<E>
    {
        cs.enforce(
            || "number assertion constraint",
            |zero| zero + self.variable - (number.clone(), CS::one()),
            |zero| zero + CS::one(),
            |zero| zero
        );

        Ok(())
    }
    /// Takes two allocated numbers (a, b) and returns
    /// (b, a) if the condition is true, and (a, b)
    /// otherwise.
    pub fn conditionally_reverse<CS>(
        mut cs: CS,
        a: &Self,
        b: &Self,
        condition: &Boolean
    ) -> Result<(Self, Self), SynthesisError>
        where CS: ConstraintSystem<E>
    {
        let c = Self::alloc(
            cs.namespace(|| "conditional reversal result 1"),
            || {
                if *condition.get_value().get()? {
                    Ok(*b.value.get()?)
                } else {
                    Ok(*a.value.get()?)
                }
            }
        )?;

        cs.enforce(
            || "first conditional reversal",
            |zero| zero + a.variable - b.variable,
            |_| condition.lc(CS::one(), E::Fr::one()),
            |zero| zero + a.variable - c.variable
        );

        let d = Self::alloc(
            cs.namespace(|| "conditional reversal result 2"),
            || {
                if *condition.get_value().get()? {
                    Ok(*a.value.get()?)
                } else {
                    Ok(*b.value.get()?)
                }
            }
        )?;

        cs.enforce(
            || "second conditional reversal",
            |zero| zero + b.variable - a.variable,
            |_| condition.lc(CS::one(), E::Fr::one()),
            |zero| zero + b.variable - d.variable
        );

        Ok((c, d))
    }

    /// Takes two allocated numbers (a, b) and returns
    /// a if the condition is true, and b
    /// otherwise.
    /// Most often to be used with b = 0
    pub fn conditionally_select<CS>(
        mut cs: CS,
        a: &Self,
        b: &Self,
        condition: &Boolean
    ) -> Result<Self, SynthesisError>
        where CS: ConstraintSystem<E>
    {
        let c = Self::alloc(
            cs.namespace(|| "conditional select result"),
            || {
                if *condition.get_value().get()? {
                    Ok(*a.value.get()?)
                } else {
                    Ok(*b.value.get()?)
                }
            }
        )?;

        // a * condition + b*(1-condition) = c ->
        // a * condition - b*condition = c - b

        cs.enforce(
            || "conditional select constraint",
            |zero| zero + a.variable - b.variable,
            |_| condition.lc(CS::one(), E::Fr::one()),
            |zero| zero + c.variable - b.variable
        );

        Ok(c)
    }

    /// Takes two allocated numbers (a, b) and returns
    /// allocated boolean variable with value `true`
    /// if the `a` and `b` are equal, `false` otherwise.
    pub fn equals<CS>(
        mut cs: CS,
        a: &Self,
        b: &Self
    ) -> Result<boolean::AllocatedBit, SynthesisError>
        where E: Engine,
            CS: ConstraintSystem<E>
    {
        // Allocate and constrain `r`: result boolean bit. 
        // It equals `true` if `a` equals `b`, `false` otherwise

        let r_value = match (a.value, b.value) {
            (Some(a), Some(b))  => Some(a == b),
            _                   => None,
        };

        let r = boolean::AllocatedBit::alloc(cs.namespace(|| "r"), r_value)?;

        // Let `delta = a - b`

        let delta_value = match (a.value, b.value) {
            (Some(a), Some(b))  => {
                // return (a - b)
                let mut a = a;
                a.sub_assign(&b);
                Some(a)
            },
            _ => None,
        };

        let delta_inv_value = delta_value.as_ref().map(|delta_value| {
            let tmp = delta_value.clone(); 
            if tmp.is_zero() {
                E::Fr::one() // we can return any number here, it doesn't matter
            } else {
                tmp.inverse().unwrap()
            }
        });

        let delta_inv = Self::alloc(cs.namespace(|| "delta_inv"), || delta_inv_value.grab() )?;

        // Allocate `t = delta * delta_inv`
        // If `delta` is non-zero (a != b), `t` will equal 1
        // If `delta` is zero (a == b), `t` cannot equal 1

        let t_value = match (delta_value, delta_inv_value) {
            (Some(a), Some(b))  => {
                let mut t = a.clone();
                t.mul_assign(&b);
                Some(t)
            },
            _ => None,
        };

        let t = Self::alloc(cs.namespace(|| "t"), || t_value.grab() )?;

        // Constrain allocation: 
        // t = (a - b) * delta_inv
        cs.enforce(
            || "t = (a - b) * delta_inv",
            |zero| zero + a.variable - b.variable,
            |zero| zero + delta_inv.variable,
            |zero| zero + t.variable,
        );

        // Constrain: 
        // (a - b) * (t - 1) == 0
        // This enforces that correct `delta_inv` was provided, 
        // and thus `t` is 1 if `(a - b)` is non zero (a != b )
        cs.enforce(
            || "(a - b) * (t - 1) == 0",
            |zero| zero + a.variable - b.variable,
            |zero| zero + t.variable - CS::one(),
            |zero| zero
        );

        // Constrain: 
        // (a - b) * r == 0
        // This enforces that `r` is zero if `(a - b)` is non-zero (a != b)
        cs.enforce(
            || "(a - b) * r == 0",
            |zero| zero + a.variable - b.variable,
            |zero| zero + r.get_variable(),
            |zero| zero
        );

        // Constrain: 
        // (t - 1) * (r - 1) == 0
        // This enforces that `r` is one if `t` is not one (a == b)
        cs.enforce(
            || "(t - 1) * (r - 1) == 0",
            |zero| zero + t.get_variable() - CS::one(),
            |zero| zero + r.get_variable() - CS::one(),
            |zero| zero
        );

        Ok(r)
    }

    /// Returns `a == b ? x : y`
    pub fn select_ifeq<CS>(
        mut cs: CS,
        a: &Self,
        b: &Self,
        x: &Self,
        y: &Self,
    ) -> Result<Self, SynthesisError>
        where E: Engine,
            CS: ConstraintSystem<E>
    {
        let eq = Self::equals(cs.namespace(|| "eq"), a, b)?;
        Self::conditionally_select(cs.namespace(|| "select"), x, y, &Boolean::from(eq))
    }

    /// Limits number of bits. The easiest example when required
    /// is to add or subtract two "small" (with bit length smaller 
    /// than one of the field) numbers and check for overflow
    pub fn limit_number_of_bits<CS>(
        &self,
        mut cs: CS,
        number_of_bits: usize
    ) -> Result<(), SynthesisError>
        where CS: ConstraintSystem<E>
    {
        // do the bit decomposition and check that higher bits are all zeros

        let mut bits = self.into_bits_le(
            cs.namespace(|| "unpack to limit number of bits")
        )?;

        bits.drain(0..number_of_bits);

        // repack

        let mut top_bits_lc = Num::<E>::zero();
        let mut coeff = E::Fr::one();
        for bit in bits.into_iter() {
            top_bits_lc = top_bits_lc.add_bool_with_coeff(CS::one(), &bit, coeff);
            coeff.double();
        }

        // enforce packing and zeroness
        cs.enforce(
            || "repack top bits",
            |zero| zero,
            |zero| zero + CS::one(),
            |_| top_bits_lc.lc(E::Fr::one())
        );

        Ok(())
    }

    pub fn get_value(&self) -> Option<E::Fr> {
        self.value
    }

    pub fn get_variable(&self) -> Variable {
        self.variable
    }
}

pub struct Num<E: Engine> {
    value: Option<E::Fr>,
    lc: LinearCombination<E>
}

impl<E: Engine> From<AllocatedNum<E>> for Num<E> {
    fn from(num: AllocatedNum<E>) -> Num<E> {
        Num {
            value: num.value,
            lc: LinearCombination::<E>::zero() + num.variable
        }
    }
}

impl<E: Engine> Clone for Num<E> {
    fn clone(&self) -> Self {
        Num {
            value: self.value.clone(),
            lc: self.lc.clone()
        }
    }
}

impl<E: Engine> Num<E> {
    pub fn zero() -> Self {
        Num {
            value: Some(E::Fr::zero()),
            lc: LinearCombination::zero()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.lc.as_ref().len() == 0
    }

    pub fn len(&self) -> usize {
        self.lc.as_ref().len()
    }


    pub fn get_value(&self) -> Option<E::Fr> {
        self.value
    }

    pub fn lc(&self, coeff: E::Fr) -> LinearCombination<E> {
        LinearCombination::zero() + (coeff, &self.lc)
    }

    pub fn add_number_with_coeff(
        self,
        variable: &AllocatedNum<E>,
        coeff: E::Fr
    ) -> Self
    {
        let newval = match (self.value, variable.get_value()) {
            (Some(mut curval), Some(val)) => {
                let mut tmp = val;
                tmp.mul_assign(&coeff);

                curval.add_assign(&tmp);

                Some(curval)
            },
            _ => None
        };

        Num {
            value: newval,
            lc: self.lc + (coeff, variable.get_variable())
        }
    }

    pub fn add_assign_number_with_coeff(
        &mut self,
        variable: &AllocatedNum<E>,
        coeff: E::Fr
    )
    {
        let newval = match (self.value, variable.get_value()) {
            (Some(mut curval), Some(val)) => {
                let mut tmp = val;
                tmp.mul_assign(&coeff);

                curval.add_assign(&tmp);

                Some(curval)
            },
            _ => None
        };

        self.value = newval;
        self.lc.as_mut().push((variable.get_variable(), coeff));
    }
   
    pub fn add_bool_with_coeff(
        self,
        one: Variable,
        bit: &Boolean,
        coeff: E::Fr
    ) -> Self
    {
        let newval = match (self.value, bit.get_value()) {
            (Some(mut curval), Some(bval)) => {
                if bval {
                    curval.add_assign(&coeff);
                }

                Some(curval)
            },
            _ => None
        };

        Num {
            value: newval,
            lc: self.lc + &bit.lc(one, coeff)
        }
    }

    pub fn add_constant(
        self,
        one: Variable,
        coeff: E::Fr
    ) -> Self
    {
        let newval = match self.value {
            Some(mut curval) => {
                curval.add_assign(&coeff);

                Some(curval)
            },
            _ => None
        };

        Num {
            value: newval,
            lc: self.lc + (coeff, one)
        }
    }

    pub fn into_allocated_num<CS: ConstraintSystem<E>>(
        self,
        mut cs: CS
    ) -> Result<AllocatedNum<E>, SynthesisError> {
        if self.lc.as_ref().len() == 1 {
            return Ok(self.unwrap_as_allocated_num());
        }
        let var = AllocatedNum::alloc(
            cs.namespace(|| "allocate a collapse result"), 
            || {
                let val = *self.get_value().get()?;

                Ok(val)
            }
        )?;

        cs.enforce(
            || "enforce collapse constraint", 
            |_| self.lc - var.get_variable(),
            |lc| lc + CS::one(),
            |lc| lc
        
        );

        Ok(var)
    }

    pub fn unwrap_as_allocated_num(
        &self,
    ) -> AllocatedNum<E> {
        assert!(self.lc.as_ref().len() == 1);
        let (var, c) = self.lc.as_ref().last().unwrap().clone();
        assert!(c == E::Fr::one());

        let var = AllocatedNum {
            value: self.value,
            variable: var
        };

        var
    }
}


impl<E: Engine> Add<&Num<E>> for Num<E> {
    type Output = Num<E>;

    fn add(self, other: &Num<E>) -> Num<E> {
        let newval = match (self.value, other.value) {
            (Some(mut curval), Some(val)) => {
                let tmp = val;
                curval.add_assign(&tmp);

                Some(curval)
            },
            _ => None
        };

        Num {
            value: newval,
            lc: self.lc + &other.lc
        }
    }
}

impl<E: Engine> Sub<&Num<E>> for Num<E> {
    type Output = Num<E>;

    fn sub(self, other: &Num<E>) -> Num<E> {
        let newval = match (self.value, other.value) {
            (Some(mut curval), Some(val)) => {
                let tmp = val;
                curval.sub_assign(&tmp);

                Some(curval)
            },
            _ => None
        };

        Num {
            value: newval,
            lc: self.lc - &other.lc
        }
    }
}

#[cfg(test)]
mod test {
    use rand::{SeedableRng, Rand, Rng, XorShiftRng};
    use bellman::{ConstraintSystem};
    use bellman::pairing::bls12_381::{Bls12, Fr};
    use bellman::pairing::ff::{Field, PrimeField, BitIterator};
    use ::circuit::test::*;
    use super::{AllocatedNum, Boolean, Num};

    #[test]
    fn test_allocated_num() {
        let mut cs = TestConstraintSystem::<Bls12>::new();

        AllocatedNum::alloc(&mut cs, || Ok(Fr::one())).unwrap();

        assert!(cs.get("num") == Fr::one());
    }

    #[test]
    fn test_num_squaring() {
        let mut cs = TestConstraintSystem::<Bls12>::new();

        let n = AllocatedNum::alloc(&mut cs, || Ok(Fr::from_str("3").unwrap())).unwrap();
        let n2 = n.square(&mut cs).unwrap();

        assert!(cs.is_satisfied());
        assert!(cs.get("squared num") == Fr::from_str("9").unwrap());
        assert!(n2.value.unwrap() == Fr::from_str("9").unwrap());
        cs.set("squared num", Fr::from_str("10").unwrap());
        assert!(!cs.is_satisfied());
    }

    #[test]
    fn test_limit_number_of_bits() {
        let mut cs = TestConstraintSystem::<Bls12>::new();

        let n = AllocatedNum::alloc(&mut cs, || Ok(Fr::from_str("3").unwrap())).unwrap();

        n.limit_number_of_bits(&mut cs, 2).unwrap();

        assert!(cs.is_satisfied());
    }

    #[test]
    fn test_limit_number_of_bits_error() {
        let mut cs = TestConstraintSystem::<Bls12>::new();

        let n = AllocatedNum::alloc(&mut cs, || Ok(Fr::from_str("3").unwrap())).unwrap();

        n.limit_number_of_bits(&mut cs, 1).unwrap();
        assert!(!cs.is_satisfied());
    }

    #[test]
    fn test_num_multiplication() {
        let mut cs = TestConstraintSystem::<Bls12>::new();

        let n = AllocatedNum::alloc(cs.namespace(|| "a"), || Ok(Fr::from_str("12").unwrap())).unwrap();
        let n2 = AllocatedNum::alloc(cs.namespace(|| "b"), || Ok(Fr::from_str("10").unwrap())).unwrap();
        let n3 = n.mul(&mut cs, &n2).unwrap();

        assert!(cs.is_satisfied());
        assert!(cs.get("product num") == Fr::from_str("120").unwrap());
        assert!(n3.value.unwrap() == Fr::from_str("120").unwrap());
        cs.set("product num", Fr::from_str("121").unwrap());
        assert!(!cs.is_satisfied());
    }

    #[test]
    fn test_num_conditional_reversal() {
        let mut rng = XorShiftRng::from_seed([0x3dbe6259, 0x8d313d76, 0x3237db17, 0xe5bc0654]);
        {
            let mut cs = TestConstraintSystem::<Bls12>::new();

            let a = AllocatedNum::alloc(cs.namespace(|| "a"), || Ok(rng.gen())).unwrap();
            let b = AllocatedNum::alloc(cs.namespace(|| "b"), || Ok(rng.gen())).unwrap();
            let condition = Boolean::constant(false);
            let (c, d) = AllocatedNum::conditionally_reverse(&mut cs, &a, &b, &condition).unwrap();

            assert!(cs.is_satisfied());

            assert_eq!(a.value.unwrap(), c.value.unwrap());
            assert_eq!(b.value.unwrap(), d.value.unwrap());
        }

        {
            let mut cs = TestConstraintSystem::<Bls12>::new();

            let a = AllocatedNum::alloc(cs.namespace(|| "a"), || Ok(rng.gen())).unwrap();
            let b = AllocatedNum::alloc(cs.namespace(|| "b"), || Ok(rng.gen())).unwrap();
            let condition = Boolean::constant(true);
            let (c, d) = AllocatedNum::conditionally_reverse(&mut cs, &a, &b, &condition).unwrap();

            assert!(cs.is_satisfied());

            assert_eq!(a.value.unwrap(), d.value.unwrap());
            assert_eq!(b.value.unwrap(), c.value.unwrap());
        }
    }

    #[test]
    fn test_num_conditional_select() {
        let mut rng = XorShiftRng::from_seed([0x3dbe6259, 0x8d313d76, 0x3237db17, 0xe5bc0654]);
        {
            let mut cs = TestConstraintSystem::<Bls12>::new();

            let a = AllocatedNum::alloc(cs.namespace(|| "a"), || Ok(rng.gen())).unwrap();
            let b = AllocatedNum::alloc(cs.namespace(|| "b"), || Ok(rng.gen())).unwrap();

            let condition_true = Boolean::constant(true);
            let c = AllocatedNum::conditionally_select(cs.namespace(|| "c"), &a, &b, &condition_true).unwrap();

            let condition_false = Boolean::constant(false);
            let d = AllocatedNum::conditionally_select(cs.namespace(|| "d"), &a, &b, &condition_false).unwrap();

            assert!(cs.is_satisfied());
            assert!(cs.num_constraints() == 2);

            assert_eq!(a.value.unwrap(), c.value.unwrap());
            assert_eq!(b.value.unwrap(), d.value.unwrap());
        }
    }

    #[test]
    fn test_num_equals() {
        let mut cs = TestConstraintSystem::<Bls12>::new();

        let a = AllocatedNum::alloc(cs.namespace(|| "a"), || Ok(Fr::from_str("10").unwrap())).unwrap();
        let b = AllocatedNum::alloc(cs.namespace(|| "b"), || Ok(Fr::from_str("12").unwrap())).unwrap();
        let c = AllocatedNum::alloc(cs.namespace(|| "c"), || Ok(Fr::from_str("10").unwrap())).unwrap();

        let not_eq = AllocatedNum::equals(cs.namespace(|| "not_eq"), &a, &b).unwrap();
        let eq = AllocatedNum::equals(cs.namespace(|| "eq"), &a, &c).unwrap();

        assert!(cs.is_satisfied());
        assert_eq!(cs.num_constraints(), 2 * 5);

        assert_eq!(not_eq.get_value().unwrap(), false);
        assert_eq!(eq.get_value().unwrap(), true);
    }

  

    #[test]
    fn select_if_equals() {
        let mut cs = TestConstraintSystem::<Bls12>::new();

        let a = AllocatedNum::alloc(cs.namespace(|| "a"), || Ok(Fr::from_str("0").unwrap())).unwrap();
        let b = AllocatedNum::alloc(cs.namespace(|| "b"), || Ok(Fr::from_str("1").unwrap())).unwrap();
        let c = AllocatedNum::alloc(cs.namespace(|| "c"), || Ok(Fr::from_str("0").unwrap())).unwrap();

        let x = AllocatedNum::alloc(cs.namespace(|| "x"), || Ok(Fr::from_str("100").unwrap())).unwrap();
        let y = AllocatedNum::alloc(cs.namespace(|| "y"), || Ok(Fr::from_str("200").unwrap())).unwrap();

        let n_eq =     AllocatedNum::select_ifeq(cs.namespace(|| "ifeq"),  &a, &c, &x, &y).unwrap();
        let n_not_eq = AllocatedNum::select_ifeq(cs.namespace(|| "ifneq"), &a, &b, &x, &y).unwrap();

        assert!(cs.is_satisfied());
        assert_eq!(n_eq.get_value().unwrap(), Fr::from_str("100").unwrap());
        assert_eq!(n_not_eq.get_value().unwrap(), Fr::from_str("200").unwrap());
    }

    #[test]
    fn test_num_nonzero() {
        {
            let mut cs = TestConstraintSystem::<Bls12>::new();

            let n = AllocatedNum::alloc(&mut cs, || Ok(Fr::from_str("3").unwrap())).unwrap();
            n.assert_nonzero(&mut cs).unwrap();

            assert!(cs.is_satisfied());
            cs.set("ephemeral inverse", Fr::from_str("3").unwrap());
            assert!(cs.which_is_unsatisfied() == Some("nonzero assertion constraint"));
        }
        {
            let mut cs = TestConstraintSystem::<Bls12>::new();

            let n = AllocatedNum::alloc(&mut cs, || Ok(Fr::zero())).unwrap();
            assert!(n.assert_nonzero(&mut cs).is_err());
        }
    }

    #[test]
    fn test_into_bits_strict() {
        let mut negone = Fr::one();
        negone.negate();

        let mut cs = TestConstraintSystem::<Bls12>::new();

        let n = AllocatedNum::alloc(&mut cs, || Ok(negone)).unwrap();
        n.into_bits_le_strict(&mut cs).unwrap();

        assert!(cs.is_satisfied());

        // make the bit representation the characteristic
        cs.set("bit 254/boolean", Fr::one());

        // this makes the conditional boolean constraint fail
        assert_eq!(cs.which_is_unsatisfied().unwrap(), "bit 254/boolean constraint");
    }

    #[test]
    fn test_into_bits() {
        let mut rng = XorShiftRng::from_seed([0x3dbe6259, 0x8d313d76, 0x3237db17, 0xe5bc0654]);

        for i in 0..200 {
            let r = Fr::rand(&mut rng);
            let mut cs = TestConstraintSystem::<Bls12>::new();

            let n = AllocatedNum::alloc(&mut cs, || Ok(r)).unwrap();

            let bits = if i % 2 == 0 {
                n.into_bits_le(&mut cs).unwrap()
            } else {
                n.into_bits_le_strict(&mut cs).unwrap()
            };

            assert!(cs.is_satisfied());

            for (b, a) in BitIterator::new(r.into_repr()).skip(1).zip(bits.iter().rev()) {
                if let &Boolean::Is(ref a) = a {
                    assert_eq!(b, a.get_value().unwrap());
                } else {
                    unreachable!()
                }
            }

            cs.set("num", Fr::rand(&mut rng));
            assert!(!cs.is_satisfied());
            cs.set("num", r);
            assert!(cs.is_satisfied());

            for i in 0..Fr::NUM_BITS {
                let name = format!("bit {}/boolean", i);
                let cur = cs.get(&name);
                let mut tmp = Fr::one();
                tmp.sub_assign(&cur);
                cs.set(&name, tmp);
                assert!(!cs.is_satisfied());
                cs.set(&name, cur);
                assert!(cs.is_satisfied());
            }
        }
    }
}
