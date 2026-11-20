// The part of ATL that OpenComposite uses, for MinGW-w64, which has no ATL: CComPtr as the
// smart pointer of WRL with the implicit conversion to a raw pointer that ATL's has.
#pragma once

#include <wrl/client.h>

template <class T>
class CComPtr : public Microsoft::WRL::ComPtr<T> {
public:
	using Microsoft::WRL::ComPtr<T>::ComPtr;
	CComPtr() = default;

	operator T*() const { return this->Get(); }
};
