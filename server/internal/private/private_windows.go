//go:build windows

package private

import (
	"fmt"
	"os"
	"strings"
	"syscall"
	"unsafe"
)

var (
	advapi32             = syscall.NewLazyDLL("advapi32.dll")
	kernel32             = syscall.NewLazyDLL("kernel32.dll")
	setNamedSecurityInfo = advapi32.NewProc("SetNamedSecurityInfoW")
	getNamedSecurityInfo = advapi32.NewProc("GetNamedSecurityInfoW")
	stringToDescriptor   = advapi32.NewProc("ConvertStringSecurityDescriptorToSecurityDescriptorW")
	descriptorToString   = advapi32.NewProc("ConvertSecurityDescriptorToStringSecurityDescriptorW")
	getDescriptorDACL    = advapi32.NewProc("GetSecurityDescriptorDacl")
	localFree            = kernel32.NewProc("LocalFree")
)

const (
	seFileObject             = 1
	daclSecurityInformation  = 0x00000004
	protectedDACLInformation = 0x80000000
	sddlRevision             = 1
)

// ownerOnly is the DACL of a directory its user alone may enter. It is
// protected from what the directories above pass on, and has one entry, full
// access for the user, inherited by every file and directory made in it.
func ownerOnly() (string, error) {
	entry, err := ownerEntry()
	if err != nil {
		return "", err
	}
	return "D:P" + entry, nil
}

func ownerEntry() (string, error) {
	token, err := syscall.OpenCurrentProcessToken()
	if err != nil {
		return "", err
	}
	defer token.Close()
	user, err := token.GetTokenUser()
	if err != nil {
		return "", err
	}
	sid, err := user.User.Sid.String()
	if err != nil {
		return "", err
	}
	return "(A;OICI;FA;;;" + sid + ")", nil
}

// restrict gives dir the owner-only DACL, which Windows passes on to what the
// directory already holds
func restrict(dir string, _ os.FileInfo) error {
	sddl, err := ownerOnly()
	if err != nil {
		return err
	}
	text, err := syscall.UTF16PtrFromString(sddl)
	if err != nil {
		return err
	}
	var descriptor uintptr
	//nolint:gosec // the pointers are made in the call's arguments, as Call requires
	converted, _, callErr := stringToDescriptor.Call(uintptr(unsafe.Pointer(text)), sddlRevision,
		uintptr(unsafe.Pointer(&descriptor)), 0)
	if converted == 0 {
		return fmt.Errorf("the owner's DACL: %w", callErr)
	}
	defer localFree.Call(descriptor) //nolint:errcheck // LocalFree fails only for a handle it never gave
	var present, defaulted int32
	var dacl uintptr
	//nolint:gosec // as above
	found, _, callErr := getDescriptorDACL.Call(descriptor, uintptr(unsafe.Pointer(&present)),
		uintptr(unsafe.Pointer(&dacl)), uintptr(unsafe.Pointer(&defaulted)))
	if found == 0 {
		return fmt.Errorf("the owner's DACL: %w", callErr)
	}
	path, err := syscall.UTF16PtrFromString(dir)
	if err != nil {
		return err
	}
	//nolint:errcheck,gosec // the call's status is its error; the pointers as above
	status, _, _ := setNamedSecurityInfo.Call(uintptr(unsafe.Pointer(path)), seFileObject,
		daclSecurityInformation|protectedDACLInformation, 0, 0, dacl, 0)
	if status != 0 {
		return fmt.Errorf("set its DACL: %w", syscall.Errno(status))
	}
	return nil
}

// Check says whether dir carries the owner-only DACL and nothing else; the
// flags Windows adds of its own, AI for a DACL set with inheritance, are not
// the directory's to answer for
//
//	D:PAI(A;OICI;FA;;;S-1-5-21-…) → protected, the owner's entry alone
func Check(dir string) error {
	want, err := ownerEntry()
	if err != nil {
		return err
	}
	got, err := daclOf(dir)
	if err != nil {
		return err
	}
	flags, entries, _ := strings.Cut(strings.TrimPrefix(got, "D:"), "(")
	if !strings.Contains(flags, "P") || "("+entries != want {
		return fmt.Errorf("%s's DACL is %s, not protected with %s alone", dir, got, want)
	}
	return nil
}

// daclOf spells dir's DACL as SDDL
func daclOf(dir string) (string, error) {
	path, err := syscall.UTF16PtrFromString(dir)
	if err != nil {
		return "", err
	}
	var descriptor, dacl uintptr
	//nolint:errcheck,gosec // the call's status is its error; the pointers as above
	status, _, _ := getNamedSecurityInfo.Call(uintptr(unsafe.Pointer(path)), seFileObject,
		daclSecurityInformation, 0, 0, uintptr(unsafe.Pointer(&dacl)), 0, uintptr(unsafe.Pointer(&descriptor)))
	if status != 0 {
		return "", fmt.Errorf("read %s's DACL: %w", dir, syscall.Errno(status))
	}
	defer localFree.Call(descriptor) //nolint:errcheck // LocalFree fails only for a handle it never gave
	var text *uint16
	var length uint32
	//nolint:gosec // as above
	spelled, _, callErr := descriptorToString.Call(descriptor, sddlRevision, daclSecurityInformation,
		uintptr(unsafe.Pointer(&text)), uintptr(unsafe.Pointer(&length)))
	if spelled == 0 {
		return "", fmt.Errorf("spell %s's DACL: %w", dir, callErr)
	}
	defer localFree.Call(uintptr(unsafe.Pointer(text))) //nolint:errcheck,gosec // as above
	spelling := unsafe.Slice(text, length)              //nolint:gosec // the text Windows made, as long as it says
	return syscall.UTF16ToString(spelling), nil
}
