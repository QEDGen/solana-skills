import QEDGen.Solana.Verify

-- ============================================================================
-- Test 1: Namespace with sorry → error
-- ============================================================================

namespace TestSorry

theorem foo : 1 + 1 = 2 := sorry
theorem bar : True := trivial

end TestSorry

/--
error: #qedgen_verify TestSorry: 1 theorem(s) still use sorry:
  - TestSorry.foo
-/
#guard_msgs in
#qedgen_verify TestSorry

-- ============================================================================
-- Test 2: Namespace with no sorry → success info
-- ============================================================================

namespace TestClean

theorem foo : 1 + 1 = 2 := by omega
theorem bar : True := trivial

end TestClean

/--
info: #qedgen_verify TestClean: all theorems verified (sorry-free)
-/
#guard_msgs in
#qedgen_verify TestClean
