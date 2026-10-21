def test_x(self):
    with self.assertRaises(ValueError):
        int('x')
